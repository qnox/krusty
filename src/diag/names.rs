//! kotlinc's diagnostic names: the identity `-Xrender-internal-diagnostic-names` prints and
//! `-Xwarning-level` selects.
//!
//! Each `releases/<version>.diagnostics.tsv` is the output of
//! `scripts/kotlinc-arguments/DumpDiagnosticNames.java` run against that release's distribution:
//! every diagnostic factory its compiler and bundled plugins declare, with the factory's severity.
//! A release has a file only where its table differs from the previous release's; a release with no
//! file reads its predecessor's ([`ReleaseRecords`]). Regenerate one with
//! `just kotlinc-arguments <version>`.
//!
//! `build.rs` turns the union of every table into one constant per name, spelled as kotlinc spells
//! it (`names::UNRESOLVED_REFERENCE`), so a report names its diagnostic by a checked identity rather
//! than by text. Whether a release declares a name, and with which severity, is that release's own
//! table: [`DiagnosticName::severity`].

use std::fmt;
use std::sync::OnceLock;

use crate::kotlin_version::{KotlinVersion, ReleaseRecords};

/// One kotlinc diagnostic, by the name its factory declares.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticName(u16);

include!(concat!(env!("OUT_DIR"), "/diagnostic_names.rs"));

/// The tables, by the release that first has each. `every_record_reads_its_own_file` keeps each
/// entry on the file named after its release.
macro_rules! releases {
    ($($version:ident => $file:literal),+ $(,)?) => {
        static RELEASES: ReleaseRecords<&str> = ReleaseRecords::new(&[$((
            KotlinVersion::$version,
            include_str!(concat!("releases/", $file, ".diagnostics.tsv")),
        )),+]);
        #[cfg(test)]
        const RELEASE_FILES: &[(KotlinVersion, &str)] = &[$((KotlinVersion::$version, $file)),+];
    };
}

releases! {
    V2_4_0 => "2.4.0",
    V2_4_20 => "2.4.20",
}

/// The vendored table text a supported release reads, for comparison with a fresh dump.
pub fn vendored_table(version: KotlinVersion) -> Option<&'static str> {
    RELEASES.get(version).copied()
}

/// kotlinc's `Severity` of a diagnostic factory.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    /// A warning `-nowarn` does not suppress.
    StrongWarning,
    Info,
}

impl DiagnosticSeverity {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "ERROR" => Self::Error,
            "WARNING" => Self::Warning,
            "STRONG_WARNING" => Self::StrongWarning,
            "INFO" => Self::Info,
            _ => return None,
        })
    }
}

impl DiagnosticName {
    /// The name as kotlinc prints it.
    pub fn as_str(self) -> &'static str {
        SPELLINGS[usize::from(self.0)]
    }

    /// The diagnostic some supported release declares under exactly this name.
    pub fn parse(name: &str) -> Option<Self> {
        let index = SPELLINGS.binary_search(&name).ok()?;
        Some(Self(
            u16::try_from(index).expect("diagnostic names fit in u16"),
        ))
    }

    /// The severity `version` declares this diagnostic with; `None` when that release does not
    /// declare it.
    pub fn severity(self, version: KotlinVersion) -> Option<DiagnosticSeverity> {
        severities(version)?[usize::from(self.0)]
    }
}

impl fmt::Debug for DiagnosticName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for DiagnosticName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Each declared name's severity in the release `version` reads, indexed like [`SPELLINGS`].
fn severities(version: KotlinVersion) -> Option<&'static [Option<DiagnosticSeverity>]> {
    static TABLES: OnceLock<Vec<(KotlinVersion, Vec<Option<DiagnosticSeverity>>)>> =
        OnceLock::new();
    let recorded = RELEASES.recorded_release(version)?;
    let tables = TABLES.get_or_init(|| {
        RELEASES
            .records()
            .iter()
            .map(|&(release, table)| {
                let parsed = parse_table(table)
                    .unwrap_or_else(|error| panic!("kotlinc {release} diagnostic table: {error}"));
                (release, parsed)
            })
            .collect()
    });
    tables
        .iter()
        .find(|(release, _)| *release == recorded)
        .map(|(_, severities)| severities.as_slice())
}

fn parse_table(table: &str) -> Result<Vec<Option<DiagnosticSeverity>>, String> {
    let mut severities = vec![None; SPELLINGS.len()];
    for row in table.lines().filter(|line| !line.starts_with('#')) {
        let (name, severity) = row
            .split_once('\t')
            .ok_or_else(|| format!("expected 2 columns: {row:?}"))?;
        let name = DiagnosticName::parse(name).ok_or_else(|| format!("unknown name {name}"))?;
        let severity = DiagnosticSeverity::parse(severity)
            .ok_or_else(|| format!("{name}: unknown severity {severity:?}"))?;
        severities[usize::from(name.0)] = Some(severity);
    }
    Ok(severities)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_release_has_a_diagnostic_table() {
        assert_eq!(RELEASES.check(), Ok(()));
        for version in KotlinVersion::supported() {
            assert_eq!(
                UNRESOLVED_REFERENCE.severity(version),
                Some(DiagnosticSeverity::Error),
                "{version}"
            );
        }
        assert_eq!(
            UNRESOLVED_REFERENCE.severity(KotlinVersion::new(2, 4, 30)),
            None
        );
    }

    #[test]
    fn every_record_reads_its_own_file() {
        for &(version, file) in RELEASE_FILES {
            assert_eq!(file, version.to_string());
        }
    }

    #[test]
    fn every_vendored_table_parses() {
        for &(version, table) in RELEASES.records() {
            parse_table(table).unwrap_or_else(|error| panic!("kotlinc {version}: {error}"));
        }
    }

    #[test]
    fn a_name_round_trips_through_its_spelling() {
        assert_eq!(UNRESOLVED_REFERENCE.as_str(), "UNRESOLVED_REFERENCE");
        assert_eq!(
            DiagnosticName::parse("UNRESOLVED_REFERENCE"),
            Some(UNRESOLVED_REFERENCE)
        );
        assert_eq!(format!("{UNRESOLVED_REFERENCE:?}"), "UNRESOLVED_REFERENCE");
        assert_eq!(DiagnosticName::parse("NOT_A_DIAGNOSTIC"), None);
        assert!(SPELLINGS.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// Recorded from kotlinc 2.4.0 and 2.4.20: `OPERATOR_RENAMED_ON_IMPORT` became a warning, and
    /// 2.4.20 removed `SYNCHRONIZED_ON_SUSPEND_WARNING`.
    #[test]
    fn a_release_reads_its_own_severity_and_names() {
        assert_eq!(
            OPERATOR_RENAMED_ON_IMPORT.severity(KotlinVersion::V2_4_10),
            Some(DiagnosticSeverity::Error)
        );
        assert_eq!(
            OPERATOR_RENAMED_ON_IMPORT.severity(KotlinVersion::V2_4_20),
            Some(DiagnosticSeverity::Warning)
        );
        assert_eq!(
            SYNCHRONIZED_ON_SUSPEND_WARNING.severity(KotlinVersion::V2_4_10),
            Some(DiagnosticSeverity::Warning)
        );
        assert_eq!(
            SYNCHRONIZED_ON_SUSPEND_WARNING.severity(KotlinVersion::V2_4_20),
            None
        );
    }
}
