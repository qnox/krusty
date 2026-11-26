//! The Kotlin reference version krusty reproduces in this process.
//!
//! krusty supports every version listed in the `kotlin-versions` manifest at once, and the reference
//! compilers do not agree on everything a user can observe. kotlinc 2.4.20, for instance, rewords
//! a batch of diagnostics and reports NO_VALUE_FOR_PARAMETER at a different position than 2.4.10.
//! A behavior that differs between reference versions is a [`Quirk`]: its releases are recorded
//! once, in the quirk table, and code tests the reproduced profile ([`ReferenceProfile::quirks`]) instead of
//! comparing versions. Diagnostic wording lives in [`crate::diagnostic_wording`], keyed by version.
//!
//! The reference profile is a value, never process state: [`ReferenceProfile`] pairs the release a
//! compilation reproduces with its defects mode, and travels with the compilation's language
//! features. Several compilations with different profiles can therefore run in one process,
//! concurrently. A driver chooses the release (a flag, [`REFERENCE_VERSION_VARIABLE`], or the newest
//! manifest entry) and builds the profile once.
//!
//! A compilation reproduces its release's defects too, unless it asks krusty to fix them
//! (`-Xfix-kotlinc-defects`, [`KotlincDefects::Fix`]).

use std::fmt;
use std::path::PathBuf;

mod quirks;

pub use quirks::{Quirk, Quirks};

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
    pub const V2_4_21: Self = Self::new(2, 4, 21);

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
        let mut versions: Vec<Self> = manifest_versions(include_str!("../../../kotlin-versions"));
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

/// Data recorded from kotlinc releases (an argument table, a feature table), kept only where it
/// changes. Each record applies from its own release up to the next record, so a supported release
/// with nothing new reads its predecessor's record and adding a release to the manifest needs no
/// copy of unchanged data. The conformance tests compare every supported release's live kotlinc
/// output with the record it reads, so an inherited record cannot hide a change.
pub struct ReleaseRecords<T: 'static> {
    records: &'static [(KotlinVersion, T)],
}

impl<T> ReleaseRecords<T> {
    /// `records` in ascending release order.
    pub const fn new(records: &'static [(KotlinVersion, T)]) -> Self {
        Self { records }
    }

    /// The record a supported release reads; `None` for a release the manifest does not list.
    pub fn get(&self, version: KotlinVersion) -> Option<&'static T> {
        let records = self.records;
        if !KotlinVersion::supported().contains(&version) {
            return None;
        }
        records
            .iter()
            .rev()
            .find(|(release, _)| *release <= version)
            .map(|(_, record)| record)
    }

    /// The release whose record `version` reads.
    pub fn recorded_release(&self, version: KotlinVersion) -> Option<KotlinVersion> {
        let records = self.records;
        self.get(version)?;
        records
            .iter()
            .rev()
            .map(|&(release, _)| release)
            .find(|release| *release <= version)
    }

    pub fn records(&self) -> &'static [(KotlinVersion, T)] {
        self.records
    }

    /// Every supported release reads a record, and every record belongs to a supported release, in
    /// ascending order.
    pub fn check(&self) -> Result<(), String> {
        let supported = KotlinVersion::supported();
        if self.records.first().map(|&(release, _)| release) != supported.first().copied() {
            return Err(format!(
                "the first record must be the oldest supported release {}",
                supported[0]
            ));
        }
        if !self.records.windows(2).all(|pair| pair[0].0 < pair[1].0) {
            return Err("records are not in ascending release order".to_string());
        }
        match self
            .records
            .iter()
            .find(|(release, _)| !supported.contains(release))
        {
            Some((release, _)) => Err(format!("{release} is not in the kotlin-versions manifest")),
            None => Ok(()),
        }
    }
}

/// The committed per-release file `version` reads, with the inheritance of [`ReleaseRecords`]: its
/// own file when it has one, else the newest supported release below it that has one. `path_of`
/// names a release's file. `None` when no release at or below `version` has a file.
pub fn recorded_file(
    version: KotlinVersion,
    path_of: impl Fn(KotlinVersion) -> PathBuf,
) -> Option<PathBuf> {
    newest_file(version, true, path_of)
}

/// The file `version` would read if it had none of its own: the newest supported release below it
/// that has one. A release records its own file only where it differs from this one.
pub fn inherited_file(
    version: KotlinVersion,
    path_of: impl Fn(KotlinVersion) -> PathBuf,
) -> Option<PathBuf> {
    newest_file(version, false, path_of)
}

fn newest_file(
    version: KotlinVersion,
    inclusive: bool,
    path_of: impl Fn(KotlinVersion) -> PathBuf,
) -> Option<PathBuf> {
    KotlinVersion::supported()
        .into_iter()
        .rev()
        .filter(|release| *release < version || (inclusive && *release == version))
        .map(path_of)
        .find(|path| path.exists())
}

/// Refuse a release the manifest does not list: krusty makes no claim about it.
pub fn validate_release(version: KotlinVersion) -> Result<KotlinVersion, String> {
    let supported = KotlinVersion::supported();
    if supported.contains(&version) {
        Ok(version)
    } else {
        let listed: Vec<String> = supported.iter().map(ToString::to_string).collect();
        Err(format!(
            "unsupported Kotlin reference version {version}; supported: {}",
            listed.join(", ")
        ))
    }
}

/// Whether a compilation reproduces the reference release's defects ([`Quirk::is_defect`]) or fixes
/// them. kotlinc is the specification, so the default reproduces them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KotlincDefects {
    #[default]
    Reproduce,
    Fix,
}

/// What one compilation reproduces of kotlinc: its reference release and whether it keeps that
/// release's defects. The quirk profile is derived once, so consumers test one bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReferenceProfile {
    release: KotlinVersion,
    defects: KotlincDefects,
    quirks: Quirks,
}

impl ReferenceProfile {
    pub fn new(release: KotlinVersion, defects: KotlincDefects) -> Self {
        Self {
            release,
            defects,
            quirks: Quirks::of_release(release).under(defects),
        }
    }

    /// `release`, reproducing its defects.
    pub fn of_release(release: KotlinVersion) -> Self {
        Self::new(release, KotlincDefects::Reproduce)
    }

    pub fn release(&self) -> KotlinVersion {
        self.release
    }

    pub fn defects(&self) -> KotlincDefects {
        self.defects
    }

    pub fn quirks(&self) -> Quirks {
        self.quirks
    }

    /// Whether the release is `version` or newer. A behavior difference is a [`Quirk`] instead;
    /// this answers the wording table and tests.
    pub fn at_least(&self, version: KotlinVersion) -> bool {
        self.release >= version
    }

    /// This profile's release, with `defects` instead.
    pub fn with_defects(self, defects: KotlincDefects) -> Self {
        Self::new(self.release, defects)
    }
}

impl Default for ReferenceProfile {
    /// The newest release krusty models, reproducing its defects.
    fn default() -> Self {
        Self::of_release(KotlinVersion::newest())
    }
}

/// The environment variable that selects the reference version when no flag does.
pub const REFERENCE_VERSION_VARIABLE: &str = "KRUSTY_LANGUAGE_VERSION";

/// The environment-selected reference release, or the newest supported one. Only a driver or test
/// harness calls this, once, to choose a compilation's [`ReferenceProfile`]; the pipeline never
/// reads the environment.
pub fn configured_target() -> Result<KotlinVersion, String> {
    match std::env::var(REFERENCE_VERSION_VARIABLE) {
        Ok(value) => target_from(Some(value)),
        Err(std::env::VarError::NotPresent) => target_from(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(
            "invalid Kotlin reference version in KRUSTY_LANGUAGE_VERSION: value is not valid UTF-8"
                .to_string(),
        ),
    }
}

/// [`configured_target`], failing loudly on an invalid environment. Reference-toolchain locators and
/// conformance harnesses, which select a provisioned kotlinc by the environment's release, use this;
/// a compilation never does: it carries its own [`ReferenceProfile`].
pub fn environment_release() -> KotlinVersion {
    configured_target().unwrap_or_else(|error| panic!("{error}"))
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
    fn a_release_reads_the_newest_record_at_or_below_it() {
        static RECORDS: ReleaseRecords<&str> = ReleaseRecords::new(&[
            (KotlinVersion::V2_4_0, "first"),
            (KotlinVersion::V2_4_20, "second"),
        ]);
        assert_eq!(RECORDS.check(), Ok(()));
        assert_eq!(RECORDS.get(KotlinVersion::V2_4_0), Some(&"first"));
        assert_eq!(RECORDS.get(KotlinVersion::V2_4_10), Some(&"first"));
        assert_eq!(RECORDS.get(KotlinVersion::V2_4_20), Some(&"second"));
        assert_eq!(RECORDS.get(KotlinVersion::newest()), Some(&"second"));
        assert_eq!(
            RECORDS.recorded_release(KotlinVersion::V2_4_10),
            Some(KotlinVersion::V2_4_0)
        );
        assert_eq!(RECORDS.get(KotlinVersion::new(2, 4, 99)), None);
        assert_eq!(RECORDS.get(KotlinVersion::new(2, 3, 0)), None);
    }

    #[test]
    fn profiles_with_different_settings_coexist_in_one_process() {
        let newest = KotlinVersion::newest();
        let reproduce = ReferenceProfile::new(newest, KotlincDefects::Reproduce);
        let fix = ReferenceProfile::new(newest, KotlincDefects::Fix);
        let older = ReferenceProfile::of_release(KotlinVersion::V2_4_0);
        assert_eq!(reproduce.defects(), KotlincDefects::Reproduce);
        assert_eq!(fix.defects(), KotlincDefects::Fix);
        assert_eq!(reproduce.release(), fix.release());
        assert_eq!(older.release(), KotlinVersion::V2_4_0);
        assert_eq!(fix.quirks(), reproduce.quirks().under(KotlincDefects::Fix));
        assert_eq!(fix.with_defects(KotlincDefects::Reproduce), reproduce);
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
