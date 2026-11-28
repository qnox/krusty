//! `// KOTLINC_VERSION:` — the kotlinc releases whose behavior a box test pins.
//!
//! A repository-owned regression for a kotlinc bug fixed in a later release cannot use an earlier
//! kotlinc as its oracle: that compiler still miscompiles the program. The directive bounds the
//! reference releases the case applies to, in interval notation with either bound optional:
//! `[2.5.0,)` (from 2.5.0), `[2.4.0,2.5.0)` (2.4.x only), `(,2.5.0]`. A bare version, `2.5.0`, is
//! the same as `[2.5.0,)`. A bound is a plain `major.minor.patch` and compares only that, so a
//! release candidate of 2.5.0 is inside `[2.5.0,)`, as it is for a
//! [`crate::kotlin_version::Quirk`] fixed in 2.5.0.

use crate::kotlin_version::KotlinVersion;

const DIRECTIVE: &str = "KOTLINC_VERSION";

/// One end of a release range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bound {
    version: KotlinVersion,
    inclusive: bool,
}

/// The releases a `// KOTLINC_VERSION:` payload admits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReleaseRange {
    from: Option<Bound>,
    to: Option<Bound>,
}

impl ReleaseRange {
    fn parse(payload: &str) -> Result<Self, String> {
        let payload = payload.trim();
        let invalid = || {
            format!(
                "`// {DIRECTIVE}: {payload}` is not a version or an interval such as `[2.5.0,)`"
            )
        };
        // A bound is a plain `major.minor.patch`: it compares only those, so a pre-release
        // qualifier (`2.5.0-RC`) would promise a distinction the range cannot make.
        let version = |text: &str| {
            let plain = text.split('.').count() == 3
                && text
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
            plain
                .then(|| KotlinVersion::parse(text))
                .flatten()
                .ok_or_else(invalid)
        };
        let Some(open) = payload.chars().next().filter(|c| matches!(c, '[' | '(')) else {
            let version = version(payload)?;
            return Ok(Self {
                from: Some(Bound {
                    version,
                    inclusive: true,
                }),
                to: None,
            });
        };
        let close = payload.chars().last().filter(|c| matches!(c, ']' | ')'));
        let (Some(close), true) = (close, payload.len() >= 2) else {
            return Err(invalid());
        };
        let (lower, upper) = payload[1..payload.len() - 1]
            .split_once(',')
            .ok_or_else(invalid)?;
        let bound = |text: &str, inclusive: bool| -> Result<Option<Bound>, String> {
            let text = text.trim();
            if text.is_empty() {
                return Ok(None);
            }
            Ok(Some(Bound {
                version: version(text)?,
                inclusive,
            }))
        };
        let range = Self {
            from: bound(lower, open == '[')?,
            to: bound(upper, close == ']')?,
        };
        if range.from.is_none() && range.to.is_none() {
            return Err(invalid());
        }
        // An interval no release can be in would drop the case from every lane as silently as a
        // typo; it is a malformed directive.
        if let (Some(from), Some(to)) = (range.from, range.to) {
            let (from_key, to_key) = (release_key(from.version), release_key(to.version));
            if from_key > to_key || (from_key == to_key && !(from.inclusive && to.inclusive)) {
                return Err(format!("`// {DIRECTIVE}: {payload}` admits no release"));
            }
        }
        Ok(range)
    }

    fn contains(self, release: KotlinVersion) -> bool {
        let release = release_key(release);
        let above = self.from.is_none_or(|from| {
            let from_key = release_key(from.version);
            release > from_key || (from.inclusive && release == from_key)
        });
        let below = self.to.is_none_or(|to| {
            let to_key = release_key(to.version);
            release < to_key || (to.inclusive && release == to_key)
        });
        above && below
    }
}

/// The `major.minor.patch` a bound compares, whatever pre-release a version carries.
fn release_key(version: KotlinVersion) -> (u16, u16, u16) {
    (version.major, version.minor, version.patch)
}

/// Whether `src` applies to the kotlinc `release`: true without a `// KOTLINC_VERSION:` line. A
/// malformed or repeated directive is an error, so a typo never silently drops a regression.
pub fn kotlinc_release_applicable(src: &str, release: KotlinVersion) -> Result<bool, String> {
    let mut payloads = src
        .lines()
        .filter_map(|line| super::directive_payload(line, DIRECTIVE));
    let Some(payload) = payloads.next() else {
        return Ok(true);
    };
    if payloads.next().is_some() {
        return Err(format!("more than one `// {DIRECTIVE}:` line"));
    }
    Ok(ReleaseRange::parse(payload)?.contains(release))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applies(payload: &str, release: KotlinVersion) -> bool {
        kotlinc_release_applicable(&format!("// {DIRECTIVE}: {payload}\n"), release)
            .expect("well-formed directive")
    }

    const V2_5_0: KotlinVersion = KotlinVersion::new(2, 5, 0);
    const V2_5_10: KotlinVersion = KotlinVersion::new(2, 5, 10);

    #[test]
    fn a_case_without_the_directive_applies_to_every_release() {
        assert_eq!(
            kotlinc_release_applicable("fun box() = \"OK\"\n", KotlinVersion::new(2, 4, 0)),
            Ok(true)
        );
    }

    #[test]
    fn a_lower_bound_admits_that_release_and_later() {
        for payload in ["2.5.0", "[2.5.0,)", "[2.5.0, )"] {
            assert!(!applies(payload, KotlinVersion::V2_4_21), "{payload}");
            assert!(applies(payload, V2_5_0), "{payload}");
            assert!(applies(payload, V2_5_10), "{payload}");
        }
        assert!(!applies("(2.5.0,)", V2_5_0));
        assert!(applies("(2.5.0,)", V2_5_10));
    }

    #[test]
    fn an_upper_bound_admits_releases_before_it() {
        assert!(applies("[2.4.0,2.5.0)", KotlinVersion::new(2, 4, 0)));
        assert!(applies("[2.4.0,2.5.0)", KotlinVersion::V2_4_21));
        assert!(!applies("[2.4.0,2.5.0)", V2_5_0));
        assert!(applies("(,2.4.20]", KotlinVersion::new(2, 4, 20)));
        assert!(!applies("(,2.4.20]", KotlinVersion::V2_4_21));
        assert!(applies("[2.5.0,2.5.0]", V2_5_0));
    }

    #[test]
    fn malformed_and_repeated_directives_are_errors() {
        for payload in [
            "",
            "2.5",
            "2.5.0-RC",
            "[2.5.0-RC,)",
            "[2.5.0",
            "[,)",
            "[2.5.0;2.6.0)",
            "x",
            "[2.5.0,2.4.0)",
            "[2.5.0,2.5.0)",
            "(2.5.0,2.5.0]",
        ] {
            assert!(
                kotlinc_release_applicable(&format!("// {DIRECTIVE}: {payload}\n"), V2_5_0)
                    .is_err(),
                "{payload:?}"
            );
        }
        assert!(kotlinc_release_applicable(
            &format!("// {DIRECTIVE}: 2.5.0\n// {DIRECTIVE}: 2.4.0\n"),
            V2_5_0
        )
        .is_err());
    }
}
