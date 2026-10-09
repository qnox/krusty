//! kotlinc's `LanguageFeature` table, one per supported reference release.
//!
//! Each `releases/<version>.features.tsv` is the output of
//! `scripts/kotlinc-arguments/DumpLanguageFeatures.java` run against that release's
//! `kotlin-compiler.jar`, in declaration order. A table is named after the first release that
//! declares it; a later release with the same features reuses that file. Regenerate it with
//! `just kotlinc-arguments <version>`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::LanguageVersionPolicy;
use crate::kotlin_version::KotlinVersion;
use crate::language_version::LanguageVersion;

/// The feature tables, by reference version. A release whose table is identical to an earlier
/// one's shares that file instead of a copy. `every_supported_release_has_a_feature_table` keeps
/// this list equal to the `kotlin-versions` manifest.
const RELEASES: &[(KotlinVersion, &str)] = &[
    (
        KotlinVersion::V2_4_0,
        include_str!("releases/2.4.0.features.tsv"),
    ),
    (
        KotlinVersion::V2_4_10,
        include_str!("releases/2.4.0.features.tsv"),
    ),
    (
        KotlinVersion::V2_4_20,
        include_str!("releases/2.4.20.features.tsv"),
    ),
];

/// The vendored table text of one release, for comparison with a fresh dump.
pub fn vendored_table(version: KotlinVersion) -> Option<&'static str> {
    RELEASES
        .iter()
        .find_map(|&(release, table)| (release == version).then_some(table))
}

/// kotlinc's `LanguageFeatureBehaviorAfterSinceVersion`: whether a feature may still be disabled
/// once its `sinceVersion` is reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BehaviorAfterSinceVersion {
    CannotBeDisabled,
    CanStillBeDisabledForNow,
}

/// One `LanguageFeature` entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageFeature {
    pub name: String,
    /// The language version that enables the feature by default; `None` keeps it off at every
    /// language version.
    pub since_version: Option<LanguageVersion>,
    /// The API version the feature additionally needs before it is on by default.
    pub since_api_version: LanguageVersion,
    /// `actuallyEnabledInProgressiveMode`: `-progressive` enables it.
    pub progressive: bool,
    pub forces_pre_release_binaries: bool,
    pub forces_pre_release_binaries_before: Option<LanguageVersion>,
    pub test_only: bool,
    pub behavior_after_since_version: BehaviorAfterSinceVersion,
    /// The name kotlinc's diagnostics quote: `context parameters` for `ContextParameters`.
    pub presentable_name: String,
    /// The page kotlinc's diagnostics point to for the feature.
    pub hint_url: Option<String>,
    /// The argument kotlinc's diagnostics name for enabling the feature (its `@Enables` argument,
    /// `-Xcontext-parameters`); `None` leaves `-XXLanguage:+Feature`.
    pub flag: Option<String>,
}

impl LanguageFeature {
    fn parse_row(row: &str) -> Result<Self, String> {
        let columns: Vec<&str> = row.split('\t').collect();
        let [name, since, since_api, progressive, pre_release, pre_release_before, test_only, behavior, hint_url, flag, presentable_name] =
            columns[..]
        else {
            return Err(format!("expected 11 columns: {row:?}"));
        };
        let optional_text = |text: &str| (!text.is_empty()).then(|| text.to_string());
        let version = |text: &str| {
            LanguageVersion::from_major_minor_text(text)
                .ok_or_else(|| format!("{name}: invalid version '{text}'"))
        };
        let optional_version = |text: &str| (!text.is_empty()).then(|| version(text)).transpose();
        let marker = |text: &str, expected: &str| match text {
            "" => Ok(false),
            text if text == expected => Ok(true),
            other => Err(format!(
                "{name}: expected '{expected}' or nothing, found '{other}'"
            )),
        };
        let behavior_after_since_version = match behavior {
            "CannotBeDisabled" => BehaviorAfterSinceVersion::CannotBeDisabled,
            "CanStillBeDisabledForNow" => BehaviorAfterSinceVersion::CanStillBeDisabledForNow,
            other => return Err(format!("{name}: unknown behavior '{other}'")),
        };
        Ok(Self {
            name: name.to_string(),
            since_version: optional_version(since)?,
            since_api_version: version(since_api)?,
            progressive: marker(progressive, "progressive")?,
            forces_pre_release_binaries: marker(pre_release, "prerelease")?,
            forces_pre_release_binaries_before: optional_version(pre_release_before)?,
            test_only: marker(test_only, "testOnly")?,
            behavior_after_since_version,
            presentable_name: presentable_name.to_string(),
            hint_url: optional_text(hint_url),
            flag: optional_text(flag),
        })
    }

    /// kotlinc's `isEnabledByDefault`: the language and API versions both reach the feature's.
    pub fn is_enabled_by_default(
        &self,
        language_version: LanguageVersion,
        api_version: LanguageVersion,
    ) -> bool {
        self.since_version
            .is_some_and(|since| language_version >= since && api_version >= self.since_api_version)
    }

    /// kotlinc's `forcesPreReleaseBinariesIfEnabled`: enabling a feature that no stable language
    /// version has released marks the output as pre-release, unless the feature declares the
    /// language version after which it no longer does.
    pub fn forces_pre_release_binaries_if_enabled(
        &self,
        versions: &LanguageVersionPolicy,
        language_version: LanguageVersion,
    ) -> bool {
        let released = self
            .since_version
            .is_some_and(|since| versions.is_stable(since));
        !released
            && self.forces_pre_release_binaries
            && self
                .forces_pre_release_binaries_before
                .is_none_or(|before| language_version <= before)
    }
}

/// The language features of one kotlinc release, in declaration order.
pub struct FeatureTable {
    features: Vec<LanguageFeature>,
    by_name: BTreeMap<String, usize>,
}

impl FeatureTable {
    fn parse(table: &str) -> Result<Self, String> {
        let features = table
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(LanguageFeature::parse_row)
            .collect::<Result<Vec<_>, _>>()?;
        let by_name = features
            .iter()
            .enumerate()
            .map(|(index, feature)| (feature.name.clone(), index))
            .collect();
        Ok(Self { features, by_name })
    }

    /// The table for one supported reference release.
    pub fn for_version(version: KotlinVersion) -> Option<&'static FeatureTable> {
        static TABLES: OnceLock<BTreeMap<KotlinVersion, FeatureTable>> = OnceLock::new();
        TABLES
            .get_or_init(|| {
                RELEASES
                    .iter()
                    .map(|&(version, table)| {
                        let parsed = FeatureTable::parse(table).unwrap_or_else(|error| {
                            panic!("kotlinc {version} language feature table: {error}")
                        });
                        (version, parsed)
                    })
                    .collect()
            })
            .get(&version)
    }

    pub fn features(&self) -> &[LanguageFeature] {
        &self.features
    }

    /// The feature kotlinc's `LanguageFeature.fromString` finds for this exact name.
    pub fn get(&self, name: &str) -> Option<&LanguageFeature> {
        self.by_name.get(name).map(|&index| &self.features[index])
    }

    /// The features `-progressive` enables, in declaration order.
    pub fn progressive(&self) -> impl Iterator<Item = &LanguageFeature> {
        self.features.iter().filter(|feature| feature.progressive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_release_has_a_feature_table() {
        assert_eq!(
            RELEASES
                .iter()
                .map(|&(version, _)| version)
                .collect::<Vec<_>>(),
            KotlinVersion::supported()
        );
        for version in KotlinVersion::supported() {
            let table = FeatureTable::for_version(version).expect("a table");
            assert!(table.get("TypeAliases").is_some());
        }
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

    /// kotlinc's `actuallyEnabledInProgressiveMode` requires a `sinceVersion`.
    #[test]
    fn a_progressive_feature_has_a_since_version() {
        for version in KotlinVersion::supported() {
            let table = FeatureTable::for_version(version).expect("a table");
            assert!(table
                .progressive()
                .all(|feature| feature.since_version.is_some()));
        }
    }

    /// `EnumEntries` is a language 1.9 feature that also needs API 1.8.
    #[test]
    fn a_default_needs_both_the_language_and_the_api_version() {
        let table = FeatureTable::for_version(KotlinVersion::V2_4_20).unwrap();
        let feature = table.get("EnumEntries").unwrap();
        assert_eq!(feature.since_version, Some(LanguageVersion::new(1, 9)));
        assert_eq!(feature.since_api_version, LanguageVersion::new(1, 8));
        assert!(feature.is_enabled_by_default(LanguageVersion::V2_0, LanguageVersion::V2_0));
        assert!(!feature.is_enabled_by_default(LanguageVersion::V2_0, LanguageVersion::new(1, 7)));
        let unreleased = table.get("ContextReceivers").unwrap();
        assert!(!unreleased.is_enabled_by_default(LanguageVersion::V2_6, LanguageVersion::V2_6));
    }

    /// kotlinc's `LanguageFeatureMessageRenderer` names a feature by its presentable name and the
    /// argument that enables it, or `-XXLanguage:+Feature` when no argument does.
    #[test]
    fn a_feature_carries_the_words_kotlinc_reports_it_by() {
        let table = FeatureTable::for_version(KotlinVersion::V2_4_20).unwrap();
        let feature = table.get("ContextParameters").unwrap();
        assert_eq!(feature.presentable_name, "context parameters");
        assert_eq!(feature.flag.as_deref(), Some("-Xcontext-parameters"));
        assert_eq!(table.get("TypeAliases").unwrap().flag, None);
    }

    /// Measured with kotlinc 2.4.20: `-XXLanguage:+CompanionBlocksAndExtensions` warns that it
    /// forces pre-release binaries; `+NameBasedDestructuring` (released in 2.5, no marker) does not.
    #[test]
    fn only_unreleased_marked_features_force_pre_release_binaries() {
        let table = FeatureTable::for_version(KotlinVersion::V2_4_20).unwrap();
        let versions = LanguageVersionPolicy::for_release(KotlinVersion::V2_4_20).unwrap();
        let forces = |name: &str, language_version| {
            table
                .get(name)
                .unwrap()
                .forces_pre_release_binaries_if_enabled(versions, language_version)
        };
        assert!(forces(
            "CompanionBlocksAndExtensions",
            LanguageVersion::V2_4
        ));
        assert!(forces(
            "CompanionBlocksAndExtensions",
            LanguageVersion::V2_5
        ));
        assert!(!forces(
            "CompanionBlocksAndExtensions",
            LanguageVersion::V2_6
        ));
        assert!(!forces("NameBasedDestructuring", LanguageVersion::V2_4));
        assert!(!forces("NestedTypeAliases", LanguageVersion::V2_4));
        assert!(forces(
            "AllowReturnsResultOfContract",
            LanguageVersion::V2_4
        ));
    }
}
