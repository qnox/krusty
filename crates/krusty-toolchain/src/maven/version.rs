//! Versions as dependency resolution reads them: a Gradle module's rich version
//! (`strictly`/`requires`/`prefers`), and a Maven range cut down to the single version the toolchain
//! picks from it (`resolveSingleVersion`).

/// A dependency's version in Gradle module metadata, or a constraint's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct RichVersion {
    pub strictly: Option<String>,
    pub requires: Option<String>,
    pub prefers: Option<String>,
}

impl RichVersion {
    pub fn requires(version: &str) -> Self {
        Self {
            requires: Some(version.to_string()),
            ..Self::default()
        }
    }

    /// The version this one stands for (`Version.resolve`): `strictly` is treated as `requires`.
    pub fn resolve(&self) -> Option<String> {
        [&self.strictly, &self.requires, &self.prefers]
            .into_iter()
            .flatten()
            .find_map(|version| single_version(version))
    }

    /// How a constraint prints it (`Version.asString`).
    pub fn render(&self) -> String {
        match &self.strictly {
            Some(strictly) => format!("{{strictly {strictly}}} -> {strictly}"),
            None => self.resolve().unwrap_or_else(|| "null".to_string()),
        }
    }
}

/// The single version a version or range stands for, or `None` for a range the toolchain cannot
/// reduce: `[1.0]` is `1.0`, `[1.0,2.0)` is its lower bound, `(,2.0]` its upper bound.
pub fn single_version(version: &str) -> Option<String> {
    let single = if version.starts_with('[') && version.ends_with(']') && !version.contains(',') {
        &version[1..version.len() - 1]
    } else {
        version
    };
    let reduced = if single.starts_with('[') && single.contains(',') {
        &single[1..single.find(',').expect("a comma")]
    } else if single.ends_with(']') && single.contains(',') {
        &single[single.rfind(',').expect("a comma") + 1..single.len() - 1]
    } else {
        single
    };
    (!reduced.starts_with('[') && !reduced.starts_with(']')).then(|| reduced.to_string())
}

#[cfg(test)]
mod tests {
    use super::{single_version, RichVersion};

    #[test]
    fn ranges_reduce_to_the_bound_the_toolchain_picks() {
        let cases = [
            ("1.0", Some("1.0")),
            ("[1.0]", Some("1.0")),
            ("[1.0,2.0)", Some("1.0")),
            ("[1.0,2.0]", Some("1.0")),
            ("(1.0,2.0]", Some("2.0")),
            ("(,2.0]", Some("2.0")),
            ("(1.0,2.0)", Some("(1.0,2.0)")),
        ];
        for (range, expected) in cases {
            assert_eq!(single_version(range).as_deref(), expected, "{range}");
        }
    }

    #[test]
    fn a_strict_version_renders_as_a_constraint_prints_it() {
        let strict = RichVersion {
            strictly: Some("1.2".to_string()),
            ..RichVersion::default()
        };
        assert_eq!(strict.render(), "{strictly 1.2} -> 1.2");
        assert_eq!(strict.resolve().as_deref(), Some("1.2"));
        assert_eq!(RichVersion::default().render(), "null");
    }
}
