//! The Kotlin reference version krusty reproduces in this process.
//!
//! krusty supports every version listed in the `kotlin-versions` manifest at once, and the reference
//! compilers do not agree on everything a user can observe. kotlinc 2.4.20, for instance, rewords
//! a batch of diagnostics and reports NO_VALUE_FOR_PARAMETER at a different position than 2.4.10.
//! Code whose output differs between reference versions asks [`target`] instead of assuming
//! one version. The wording itself lives in [`crate::diagnostic_wording`], keyed by version.
//!
//! The target is process-wide, like the reference toolchain [`crate::toolchain`] already selects:
//! the `-Xkotlin-reference-version=` flag ([`set_target`]) wins, then `KRUSTY_LANGUAGE_VERSION`
//! (already part of the build-cache key), then the newest manifest entry. A worker or daemon
//! therefore serves one reference version for its lifetime.

use std::fmt;
use std::sync::OnceLock;

/// A full Kotlin release version, `major.minor.patch`, ordered numerically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KotlinVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl KotlinVersion {
    pub const V2_4_0: Self = Self::new(2, 4, 0);
    pub const V2_4_10: Self = Self::new(2, 4, 10);
    pub const V2_4_20: Self = Self::new(2, 4, 20);

    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse `major.minor.patch`; anything else (a qualifier, a missing part) is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let mut next = || parts.next()?.parse().ok();
        let version = Self::new(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    /// Every reference version the manifest lists, oldest first.
    pub fn supported() -> Vec<Self> {
        let mut versions: Vec<Self> = manifest_versions(include_str!("../kotlin-versions"));
        versions.sort();
        versions
    }

    /// The newest reference version the manifest lists: krusty's headline version.
    pub fn newest() -> Self {
        Self::supported()
            .pop()
            .expect("the kotlin-versions manifest lists at least one reference version")
    }
}

impl fmt::Display for KotlinVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn manifest_versions(manifest: &str) -> Vec<KotlinVersion> {
    manifest
        .lines()
        .filter_map(|line| line.split('#').next()?.split_whitespace().next())
        .map(|version| {
            KotlinVersion::parse(version).unwrap_or_else(|| {
                panic!("invalid reference version {version:?} in kotlin-versions")
            })
        })
        .collect()
}

static TARGET: OnceLock<KotlinVersion> = OnceLock::new();

/// Fix the reference version for this process. Must run before anything reads [`target`]; a
/// version the manifest does not list is refused, since krusty makes no claim about it.
pub fn set_target(version: KotlinVersion) -> Result<(), String> {
    let supported = KotlinVersion::supported();
    if !supported.contains(&version) {
        let listed: Vec<String> = supported.iter().map(ToString::to_string).collect();
        return Err(format!(
            "unsupported Kotlin reference version {version}; supported: {}",
            listed.join(", ")
        ));
    }
    let chosen = *TARGET.get_or_init(|| version);
    if chosen == version {
        Ok(())
    } else {
        Err(format!(
            "the Kotlin reference version is already {chosen} in this process; cannot switch to {version}"
        ))
    }
}

/// The reference version this process reproduces.
pub fn target() -> KotlinVersion {
    *TARGET.get_or_init(|| configured_target().unwrap_or_else(|error| panic!("{error}")))
}

/// Validate the environment-selected reference version without fixing the process-wide target.
/// Drivers use this to turn a bad build configuration into an ordinary compilation error; library
/// callers that read [`target`] directly still fail loudly instead of silently compiling as a
/// different Kotlin release.
pub fn configured_target() -> Result<KotlinVersion, String> {
    match std::env::var("KRUSTY_LANGUAGE_VERSION") {
        Ok(value) => target_from(Some(value)),
        Err(std::env::VarError::NotPresent) => target_from(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(
            "invalid Kotlin reference version in KRUSTY_LANGUAGE_VERSION: value is not valid UTF-8"
                .to_string(),
        ),
    }
}

/// Whether the reference version is `version` or newer.
pub fn at_least(version: KotlinVersion) -> bool {
    target() >= version
}

fn target_from(env: Option<String>) -> Result<KotlinVersion, String> {
    let Some(value) = env else {
        return Ok(KotlinVersion::newest());
    };
    let version = KotlinVersion::parse(&value).ok_or_else(|| {
        format!("invalid Kotlin reference version {value:?}; expected major.minor.patch")
    })?;
    let supported = KotlinVersion::supported();
    if supported.contains(&version) {
        Ok(version)
    } else {
        let listed = supported
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        Err(format!(
            "unsupported Kotlin reference version {version}; supported: {}",
            listed.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_numerically_not_lexically() {
        let v = |text| KotlinVersion::parse(text).unwrap();
        assert!(v("2.4.10") > v("2.4.9"));
        assert!(v("2.4.20") > v("2.4.10"));
        assert!(v("2.10.0") > v("2.4.20"));
        assert_eq!(v("2.4.20").to_string(), "2.4.20");
    }

    #[test]
    fn only_a_full_release_version_parses() {
        for text in ["2.4", "2.4.20-RC", "2.4.20.1", "", "x.y.z"] {
            assert_eq!(KotlinVersion::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_manifest_is_the_supported_set() {
        let supported = KotlinVersion::supported();
        assert!(supported.contains(&KotlinVersion::V2_4_10));
        assert!(supported.contains(&KotlinVersion::V2_4_20));
        assert_eq!(KotlinVersion::newest(), *supported.last().unwrap());
        assert_eq!(
            manifest_versions("# c\n2.4.0 a\n\n2.4.10 b # note\n"),
            [KotlinVersion::V2_4_0, KotlinVersion::V2_4_10]
        );
    }

    #[test]
    fn the_environment_selects_the_target_and_defaults_to_the_newest() {
        assert_eq!(
            target_from(Some("2.4.10".into())),
            Ok(KotlinVersion::V2_4_10)
        );
        assert_eq!(target_from(None), Ok(KotlinVersion::newest()));
        assert!(target_from(Some(String::new())).is_err());
        assert!(target_from(Some("2.4.99".into())).is_err());
    }
}
