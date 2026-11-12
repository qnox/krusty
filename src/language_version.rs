//! Kotlin source-language levels selected by the public `-language-version` option.
//!
//! This is deliberately separate from [`crate::kotlin_version`]. A Kotlin compiler release such
//! as 2.4.20 can compile several source-language levels; the release selects reference wording and
//! bytecode details, while this value selects source semantics and the default metadata stamp.

use std::fmt;

use crate::features::LanguageVersionPolicy;
use crate::kotlin_version::KotlinVersion;

/// A stable Kotlin source-language level, written as `major.minor` on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageVersion {
    pub major: u16,
    pub minor: u16,
}

impl LanguageVersion {
    pub const V2_0: Self = Self::new(2, 0);
    pub const V2_1: Self = Self::new(2, 1);
    pub const V2_2: Self = Self::new(2, 2);
    pub const V2_3: Self = Self::new(2, 3);
    pub const V2_4: Self = Self::new(2, 4);
    pub const V2_5: Self = Self::new(2, 5);
    pub const V2_6: Self = Self::new(2, 6);

    /// Metadata stamps intentionally supported by krusty's internal emission override. This is a
    /// separate contract from the public source/API levels above.
    pub const SUPPORTED_METADATA_STAMPS: [Self; 5] =
        [Self::V2_0, Self::V2_1, Self::V2_2, Self::V2_3, Self::V2_4];

    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// A `major.minor` level, including historical levels outside the selected compiler's option
    /// domain. `@SinceKotlin("1.4")` is such a level: it is a declaration fact, not a CLI value.
    pub(crate) fn from_major_minor_text(text: &str) -> Option<Self> {
        Self::parse(text)
    }

    fn parse(text: &str) -> Option<Self> {
        let (major, minor) = text.split_once('.')?;
        if minor.contains('.')
            || major.is_empty()
            || minor.is_empty()
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || !minor.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self::new(major.parse().ok()?, minor.parse().ok()?))
    }

    /// The language/API option domain of one concrete kotlinc release: the levels its recorded
    /// policy still supports. A release without a recorded policy has none.
    pub fn supported_for(compiler: KotlinVersion) -> &'static [Self] {
        LanguageVersionPolicy::for_release(compiler).map_or(&[], LanguageVersionPolicy::supported)
    }

    pub fn parse_supported_for(text: &str, compiler: KotlinVersion) -> Option<Self> {
        let version = Self::parse(text)?;
        Self::supported_for(compiler)
            .contains(&version)
            .then_some(version)
    }

    /// The domain as kotlinc lists it: a deprecated level marked `(deprecated)`, an unreleased one
    /// `(experimental)`.
    pub fn supported_text_for(compiler: KotlinVersion) -> String {
        let Some(policy) = LanguageVersionPolicy::for_release(compiler) else {
            return String::new();
        };
        policy
            .supported()
            .iter()
            .map(|&version| {
                let status = policy
                    .language(version)
                    .expect("a supported level is declared");
                if status.deprecated {
                    format!("{version} (deprecated)")
                } else if !status.stable {
                    format!("{version} (experimental)")
                } else {
                    version.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn parse_supported_metadata_stamp(text: &str) -> Option<Self> {
        let version = Self::parse(text)?;
        Self::SUPPORTED_METADATA_STAMPS
            .contains(&version)
            .then_some(version)
    }

    pub fn supported_metadata_stamps_text() -> String {
        Self::SUPPORTED_METADATA_STAMPS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The metadata version which kotlinc derives from this source-language level.
    pub const fn metadata_version(self) -> [i32; 3] {
        [self.major as i32, self.minor as i32, 0]
    }

    /// The standard no-option language/API default of a selected compiler release: its recorded
    /// `LanguageVersion.LATEST_STABLE`.
    pub fn default_for(compiler: KotlinVersion) -> Self {
        LanguageVersionPolicy::for_release(compiler)
            .unwrap_or_else(|| panic!("kotlinc {compiler} has no recorded language version policy"))
            .latest_stable()
    }
}

impl Default for LanguageVersion {
    fn default() -> Self {
        Self::default_for(KotlinVersion::newest())
    }
}

impl fmt::Display for LanguageVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_the_levels_supported_by_the_selected_compiler() {
        for &version in LanguageVersion::supported_for(KotlinVersion::V2_4_20) {
            assert_eq!(
                LanguageVersion::parse_supported_for(&version.to_string(), KotlinVersion::V2_4_20,),
                Some(version)
            );
        }
        for value in [
            "", "2", "2.4.0", "+2.2", "-2.2", "2.x", "1.9", "2.7", "999.1",
        ] {
            assert_eq!(
                LanguageVersion::parse_supported_for(value, KotlinVersion::V2_4_20),
                None,
                "{value:?}"
            );
        }
        assert_eq!(
            LanguageVersion::supported_text_for(KotlinVersion::V2_4_20),
            "2.0 (deprecated), 2.1 (deprecated), 2.2, 2.3, 2.4, 2.5 (experimental), 2.6 (experimental)"
        );
        assert_eq!(
            LanguageVersion::supported_text_for(KotlinVersion::V2_4_10),
            "2.0 (deprecated), 2.1 (deprecated), 2.2, 2.3, 2.4, 2.5 (experimental)"
        );
        assert_eq!(
            LanguageVersion::parse_supported_for("2.6", KotlinVersion::V2_4_10),
            None
        );
        assert_eq!(
            LanguageVersion::supported_for(KotlinVersion::new(2, 4, 30)),
            [],
            "an unrecorded compiler release must not inherit a nearby release's domain"
        );
    }

    #[test]
    fn the_internal_metadata_override_keeps_its_separate_domain() {
        assert_eq!(
            LanguageVersion::parse_supported_metadata_stamp("2.4"),
            Some(LanguageVersion::V2_4)
        );
        assert_eq!(LanguageVersion::parse_supported_metadata_stamp("2.5"), None);
        assert_eq!(
            LanguageVersion::supported_metadata_stamps_text(),
            "2.0, 2.1, 2.2, 2.3, 2.4"
        );
    }

    #[test]
    fn language_level_maps_to_the_kotlin_metadata_version() {
        assert_eq!(LanguageVersion::V2_2.metadata_version(), [2, 2, 0]);
        assert_eq!(LanguageVersion::default(), LanguageVersion::V2_4);
        assert_eq!(
            LanguageVersion::default_for(KotlinVersion::V2_4_0),
            LanguageVersion::V2_4
        );
    }
}
