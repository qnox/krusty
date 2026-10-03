//! Kotlin source-language levels selected by the public `-language-version` option.
//!
//! This is deliberately separate from [`crate::kotlin_version`]. A Kotlin compiler release such
//! as 2.4.20 can compile several source-language levels; the release selects reference wording and
//! bytecode details, while this value selects source semantics and the default metadata stamp.

use std::fmt;

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

    pub const SUPPORTED: [Self; 5] = [Self::V2_0, Self::V2_1, Self::V2_2, Self::V2_3, Self::V2_4];

    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Parse one of the stable levels supported by the selected 2.4 compiler line.
    pub fn parse_supported(text: &str) -> Option<Self> {
        let (major, minor) = text.split_once('.')?;
        if minor.contains('.')
            || major.is_empty()
            || minor.is_empty()
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || !minor.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let version = Self::new(major.parse().ok()?, minor.parse().ok()?);
        Self::SUPPORTED.contains(&version).then_some(version)
    }

    pub fn supported_text() -> String {
        Self::SUPPORTED
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The metadata version which kotlinc derives from this source-language level.
    pub const fn metadata_version(self) -> [i32; 3] {
        [self.major as i32, self.minor as i32, 0]
    }
}

impl Default for LanguageVersion {
    fn default() -> Self {
        Self::V2_4
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
    fn parses_only_the_supported_stable_levels() {
        for version in LanguageVersion::SUPPORTED {
            assert_eq!(
                LanguageVersion::parse_supported(&version.to_string()),
                Some(version)
            );
        }
        for value in [
            "", "2", "2.4.0", "+2.2", "-2.2", "2.x", "1.9", "2.5", "999.1",
        ] {
            assert_eq!(LanguageVersion::parse_supported(value), None, "{value:?}");
        }
    }

    #[test]
    fn language_level_maps_to_the_kotlin_metadata_version() {
        assert_eq!(LanguageVersion::V2_2.metadata_version(), [2, 2, 0]);
        assert_eq!(LanguageVersion::default(), LanguageVersion::V2_4);
    }
}
