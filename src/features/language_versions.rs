//! kotlinc's `LanguageVersion` and `ApiVersion` policy, one per supported reference release: which
//! levels are stable, deprecated, or no longer supported.
//!
//! Each `releases/<version>.language-versions.tsv` is the output of
//! `scripts/kotlinc-arguments/DumpLanguageVersions.java` run against that release's
//! `kotlin-compiler.jar`, in declaration order. Like the feature tables, a later release with the
//! same policy reuses the earlier file; every release still has its own entry below. Regenerate it
//! with `just kotlinc-arguments <version>`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::kotlin_version::KotlinVersion;
use crate::language_version::LanguageVersion;

/// The version policies, by reference version. `every_supported_release_has_a_version_policy`
/// keeps this list equal to the `kotlin-versions` manifest.
const RELEASES: &[(KotlinVersion, &str)] = &[
    (
        KotlinVersion::V2_4_0,
        include_str!("releases/2.4.0.language-versions.tsv"),
    ),
    (
        KotlinVersion::V2_4_10,
        include_str!("releases/2.4.0.language-versions.tsv"),
    ),
    (
        KotlinVersion::V2_4_20,
        include_str!("releases/2.4.20.language-versions.tsv"),
    ),
];

/// The vendored policy text of one release, for comparison with a fresh dump.
pub fn vendored_language_versions(version: KotlinVersion) -> Option<&'static str> {
    RELEASES
        .iter()
        .find_map(|&(release, table)| (release == version).then_some(table))
}

/// What kotlinc says about one level, as a language version or as an API version.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VersionStatus {
    pub stable: bool,
    pub deprecated: bool,
    pub unsupported: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Level {
    version: LanguageVersion,
    language: VersionStatus,
    api: VersionStatus,
}

/// Every `LanguageVersion` one kotlinc release declares, in declaration (ascending) order.
pub struct LanguageVersionPolicy {
    levels: Vec<Level>,
    /// The levels `-language-version` and `-api-version` accept: those still supported.
    supported: Vec<LanguageVersion>,
}

impl LanguageVersionPolicy {
    fn parse(table: &str) -> Result<Self, String> {
        let levels = table
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|row| {
                let columns: Vec<&str> = row.split('\t').collect();
                let [version, stable, deprecated, unsupported, api_stable, api_deprecated, api_unsupported] =
                    columns[..]
                else {
                    return Err(format!("expected 7 columns: {row:?}"));
                };
                let marker = |text: &str, expected: &str| match text {
                    "-" => Ok(false),
                    text if text == expected => Ok(true),
                    other => Err(format!("{version}: expected '{expected}' or '-', found '{other}'")),
                };
                Ok(Level {
                    version: LanguageVersion::from_major_minor_text(version)
                        .ok_or_else(|| format!("invalid version '{version}'"))?,
                    language: VersionStatus {
                        stable: marker(stable, "stable")?,
                        deprecated: marker(deprecated, "deprecated")?,
                        unsupported: marker(unsupported, "unsupported")?,
                    },
                    api: VersionStatus {
                        stable: marker(api_stable, "stable")?,
                        deprecated: marker(api_deprecated, "deprecated")?,
                        unsupported: marker(api_unsupported, "unsupported")?,
                    },
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if !levels
            .windows(2)
            .all(|pair| pair[0].version < pair[1].version)
        {
            return Err("versions are not in ascending order".to_string());
        }
        let supported = levels
            .iter()
            .filter(|level| !level.language.unsupported)
            .map(|level| level.version)
            .collect();
        Ok(Self { levels, supported })
    }

    /// The policy of one supported reference release.
    pub fn for_release(version: KotlinVersion) -> Option<&'static LanguageVersionPolicy> {
        static POLICIES: OnceLock<BTreeMap<KotlinVersion, LanguageVersionPolicy>> = OnceLock::new();
        POLICIES
            .get_or_init(|| {
                RELEASES
                    .iter()
                    .map(|&(version, table)| {
                        let parsed = Self::parse(table).unwrap_or_else(|error| {
                            panic!("kotlinc {version} language version table: {error}")
                        });
                        (version, parsed)
                    })
                    .collect()
            })
            .get(&version)
    }

    fn level(&self, version: LanguageVersion) -> Option<&Level> {
        self.levels.iter().find(|level| level.version == version)
    }

    /// The release's status of `version` as a language version; a level it does not declare has
    /// no status.
    pub fn language(&self, version: LanguageVersion) -> Option<VersionStatus> {
        self.level(version).map(|level| level.language)
    }

    /// The release's status of `version` as an API version.
    pub fn api(&self, version: LanguageVersion) -> Option<VersionStatus> {
        self.level(version).map(|level| level.api)
    }

    /// kotlinc's `LanguageVersion.isStable`: a declared level the release has released.
    pub fn is_stable(&self, version: LanguageVersion) -> bool {
        self.language(version).is_some_and(|status| status.stable)
    }

    /// The levels `-language-version` and `-api-version` accept, ascending.
    pub fn supported(&self) -> &[LanguageVersion] {
        &self.supported
    }

    /// kotlinc's `LanguageVersion.LATEST_STABLE`: the newest stable level, which is also the level
    /// a compilation without `-language-version` uses.
    pub fn latest_stable(&self) -> LanguageVersion {
        self.levels
            .iter()
            .rev()
            .find(|level| level.language.stable)
            .expect("a release declares a stable language version")
            .version
    }

    /// kotlinc's `LanguageVersion.FIRST_NON_DEPRECATED` (`api == false`) or
    /// `ApiVersion.FIRST_NON_DEPRECATED` (`api == true`): the level a deprecation warning names.
    pub fn first_non_deprecated(&self, api: bool) -> LanguageVersion {
        self.levels
            .iter()
            .find(|level| {
                let status = if api { level.api } else { level.language };
                !status.deprecated && !status.unsupported
            })
            .expect("a release declares a supported, non-deprecated version")
            .version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_release_has_a_version_policy() {
        assert_eq!(
            RELEASES
                .iter()
                .map(|&(version, _)| version)
                .collect::<Vec<_>>(),
            KotlinVersion::supported()
        );
        for version in KotlinVersion::supported() {
            assert!(
                LanguageVersionPolicy::for_release(version).is_some(),
                "{version}"
            );
        }
        assert!(LanguageVersionPolicy::for_release(KotlinVersion::new(2, 4, 30)).is_none());
    }

    /// Recorded from kotlinc 2.4.0 and 2.4.20: 2.4 is the latest stable level, 2.0 and 2.1 are
    /// deprecated in favour of 2.2, 1.9 is no longer supported, and 2.6 exists only in 2.4.20.
    #[test]
    fn the_policy_is_the_releases_own() {
        let older = LanguageVersionPolicy::for_release(KotlinVersion::V2_4_0).unwrap();
        let newer = LanguageVersionPolicy::for_release(KotlinVersion::V2_4_20).unwrap();
        for policy in [older, newer] {
            assert_eq!(policy.latest_stable(), LanguageVersion::V2_4);
            assert_eq!(policy.first_non_deprecated(false), LanguageVersion::V2_2);
            assert_eq!(policy.first_non_deprecated(true), LanguageVersion::V2_2);
            assert!(!policy.is_stable(LanguageVersion::V2_5));
            assert!(
                policy
                    .language(LanguageVersion::new(1, 9))
                    .unwrap()
                    .unsupported
            );
        }
        assert_eq!(older.supported().last(), Some(&LanguageVersion::V2_5));
        assert_eq!(newer.supported().last(), Some(&LanguageVersion::V2_6));
        assert_eq!(older.language(LanguageVersion::V2_6), None);
    }

    #[test]
    fn no_vendored_row_ends_in_whitespace() {
        for &(version, table) in RELEASES {
            assert!(
                table.lines().all(|line| line.trim_end() == line),
                "kotlinc {version}"
            );
        }
    }
}
