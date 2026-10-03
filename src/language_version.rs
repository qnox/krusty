//! Kotlin source-language levels selected by the public `-language-version` option.
//!
//! This is deliberately separate from [`crate::kotlin_version`]. A Kotlin compiler release such
//! as 2.4.20 can compile several source-language levels; the release selects reference wording and
//! bytecode details, while this value selects source semantics and the default metadata stamp.

use std::fmt;

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

    /// Source/API levels accepted by every supported compiler in the 2.4 line. `2.5` is
    /// experimental. kotlinc 2.4.20 adds `2.6`; callers must use [`Self::supported_for`] rather
    /// than treating this common subset as the complete domain of every release.
    const SUPPORTED_THROUGH_2_4_10: [Self; 6] = [
        Self::V2_0,
        Self::V2_1,
        Self::V2_2,
        Self::V2_3,
        Self::V2_4,
        Self::V2_5,
    ];
    const SUPPORTED_FROM_2_4_20: [Self; 7] = [
        Self::V2_0,
        Self::V2_1,
        Self::V2_2,
        Self::V2_3,
        Self::V2_4,
        Self::V2_5,
        Self::V2_6,
    ];

    /// Metadata stamps intentionally supported by krusty's internal emission override. This is a
    /// separate contract from the public source/API levels above.
    pub const SUPPORTED_METADATA_STAMPS: [Self; 5] =
        [Self::V2_0, Self::V2_1, Self::V2_2, Self::V2_3, Self::V2_4];

    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
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

    /// The language/API option domain of one concrete kotlinc release.
    pub fn supported_for(compiler: KotlinVersion) -> &'static [Self] {
        if compiler >= KotlinVersion::V2_4_20 {
            &Self::SUPPORTED_FROM_2_4_20
        } else {
            &Self::SUPPORTED_THROUGH_2_4_10
        }
    }

    pub fn parse_supported_for(text: &str, compiler: KotlinVersion) -> Option<Self> {
        let version = Self::parse(text)?;
        Self::supported_for(compiler)
            .contains(&version)
            .then_some(version)
    }

    pub fn supported_text_for(compiler: KotlinVersion) -> String {
        Self::supported_for(compiler)
            .iter()
            .map(|version| match *version {
                Self::V2_0 | Self::V2_1 => format!("{version} (deprecated)"),
                Self::V2_5 | Self::V2_6 => format!("{version} (experimental)"),
                _ => version.to_string(),
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

    /// The standard no-option language/API default of a selected compiler release.
    pub const fn default_for(compiler: KotlinVersion) -> Self {
        Self::new(compiler.major, compiler.minor)
    }
}

impl Default for LanguageVersion {
    fn default() -> Self {
        Self::default_for(KotlinVersion::V2_4_20)
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
