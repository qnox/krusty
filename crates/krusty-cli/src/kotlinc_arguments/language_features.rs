//! kotlinc's `LanguageFeature` table, one per supported reference release.
//!
//! Each `releases/<version>.features.tsv` is the output of
//! `scripts/kotlinc-arguments/DumpLanguageFeatures.java` run against that release's
//! `kotlin-compiler.jar`, in declaration order. A table is named after the first release that
//! declares it; a later release with the same features reuses that file. Regenerate it with
//! `just kotlinc-arguments <version>`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use krusty::kotlin_version::KotlinVersion;

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

/// One `LanguageFeature` entry. Versions are kotlinc's `versionString`s.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageFeature {
    pub name: String,
    pub since_version: Option<String>,
    pub since_api_version: String,
    /// `actuallyEnabledInProgressiveMode`: `-progressive` enables it.
    pub progressive: bool,
    pub forces_pre_release_binaries: bool,
    pub forces_pre_release_binaries_before: Option<String>,
    pub test_only: bool,
    /// The simple class name of `behaviorAfterSinceVersion`.
    pub behavior_after_since_version: String,
}

impl LanguageFeature {
    fn parse_row(row: &str) -> Result<Self, String> {
        let columns: Vec<&str> = row.split('\t').collect();
        let [name, since, since_api, progressive, pre_release, pre_release_before, test_only, behavior] =
            columns[..]
        else {
            return Err(format!("expected 8 columns: {row:?}"));
        };
        let optional = |text: &str| (!text.is_empty()).then(|| text.to_string());
        let marker = |text: &str, expected: &str| match text {
            "" => Ok(false),
            text if text == expected => Ok(true),
            other => Err(format!(
                "{name}: expected '{expected}' or nothing, found '{other}'"
            )),
        };
        Ok(Self {
            name: name.to_string(),
            since_version: optional(since),
            since_api_version: since_api.to_string(),
            progressive: marker(progressive, "progressive")?,
            forces_pre_release_binaries: marker(pre_release, "prerelease")?,
            forces_pre_release_binaries_before: optional(pre_release_before),
            test_only: marker(test_only, "testOnly")?,
            behavior_after_since_version: behavior.to_string(),
        })
    }
}

/// The language features of one kotlinc release, in declaration order.
pub struct FeatureTable {
    features: Vec<LanguageFeature>,
}

impl FeatureTable {
    fn parse(table: &str) -> Result<Self, String> {
        let features = table
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(LanguageFeature::parse_row)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { features })
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

    /// Whether this exact reference release declares the language feature. Raw `-XXLanguage`
    /// validates against kotlinc's table rather than a second krusty-owned list.
    pub fn contains(&self, name: &str) -> bool {
        self.features.iter().any(|feature| feature.name == name)
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
            assert!(table
                .features()
                .iter()
                .any(|feature| feature.name == "TypeAliases"));
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
}
