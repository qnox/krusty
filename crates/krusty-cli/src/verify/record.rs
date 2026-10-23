//! One compilation's verify record, and the command lines of the two verify commands.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::outputs::OutputComparison;

/// What `krusty verify-compile` found for one compilation. Written as JSON beside the build's other
/// outputs and read back by `krusty verify-report`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilationRecord {
    /// The build tool's name for the compilation, such as a Gradle task path.
    pub compilation: String,
    /// The source set (`main`, `test`, …) the compilation belongs to, when the build tool says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_set: Option<String>,
    /// The Kotlin version kotlinc compiled with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kotlin_version: Option<String>,
    pub outcome: Outcome,
    /// How long kotlinc's compilation took, as the build tool measured it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kotlinc_millis: Option<u64>,
    /// How long krusty took, from its start to its written output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub krusty_millis: Option<u64>,
    /// krusty's peak resident memory over the compilation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub krusty_peak_rss_bytes: Option<u64>,
}

/// Whether krusty compiled the compilation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Outcome {
    /// krusty does not implement part of the request (a compiler plugin, an argument), so a build
    /// with the krusty plugin leaves this compilation to kotlinc. Not a failure.
    Unsupported { reasons: Vec<String> },
    /// krusty supports the request and failed to compile it: its errors, in its words.
    Failed { errors: Vec<String> },
    /// krusty compiled it; how its output compares with kotlinc's.
    Compiled { comparison: OutputComparison },
    /// The compilation has no Kotlin sources this time, so neither compiler compiled anything.
    /// Recorded so a report never carries an earlier invocation's result for it.
    NoSources,
}

/// `krusty verify-compile [options] -- <kotlinc arguments>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VerifyCompileArguments {
    /// kotlinc's output directory for the same compilation.
    pub reference: PathBuf,
    /// Where to write the [`CompilationRecord`].
    pub record: PathBuf,
    pub compilation: String,
    pub source_set: Option<String>,
    pub kotlin_version: Option<String>,
    pub kotlinc_millis: Option<u64>,
    /// Reasons the build tool already knows krusty does not support the compilation; krusty then
    /// does not run.
    pub unsupported: Vec<String>,
    /// The compilation's kotlinc arguments, with krusty's own `-d`.
    pub kotlinc_arguments: Vec<String>,
}

pub const VERIFY_COMPILE_USAGE: &str = "usage: krusty verify-compile --reference <kotlinc output> \
--record <record.json> --compilation <name> [--source-set <name>] [--kotlin-version <version>] \
[--kotlinc-millis <ms>] [--unsupported <reason>]... [--unsupported-from <file>] \
-- <kotlinc arguments with -d <krusty output>>";

impl VerifyCompileArguments {
    pub fn parse(argv: &[String]) -> Result<VerifyCompileArguments, String> {
        let mut parsed = VerifyCompileArguments::default();
        let mut reference = None;
        let mut record = None;
        let mut compilation = None;
        let mut index = 0;
        while index < argv.len() {
            let option = argv[index].as_str();
            index += 1;
            if option == "--" {
                parsed.kotlinc_arguments = argv[index..].to_vec();
                break;
            }
            let value = argv
                .get(index)
                .cloned()
                .ok_or_else(|| format!("{option} needs a value"))?;
            index += 1;
            match option {
                "--reference" => reference = Some(PathBuf::from(value)),
                "--record" => record = Some(PathBuf::from(value)),
                "--compilation" => compilation = Some(value),
                "--source-set" => parsed.source_set = Some(value),
                "--kotlin-version" => parsed.kotlin_version = Some(value),
                "--kotlinc-millis" => {
                    parsed.kotlinc_millis = Some(value.parse().map_err(|_| {
                        format!("--kotlinc-millis takes milliseconds, not '{value}'")
                    })?)
                }
                "--unsupported" => parsed.unsupported.push(value),
                // One reason per line, as the build tool's support check wrote them: a file keeps
                // them in UTF-8 whatever encoding the build tool would pass arguments in.
                "--unsupported-from" => parsed.unsupported.extend(
                    std::fs::read_to_string(&value)
                        .map_err(|error| format!("cannot read {value}: {error}"))?
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .map(str::to_string),
                ),
                _ => return Err(format!("unknown option '{option}'")),
            }
        }
        parsed.reference = reference.ok_or("--reference is required")?;
        parsed.record = record.ok_or("--record is required")?;
        parsed.compilation = compilation.ok_or("--compilation is required")?;
        if parsed.unsupported.is_empty() && parsed.kotlinc_arguments.is_empty() {
            return Err("the kotlinc arguments follow '--'".to_string());
        }
        // krusty's output is cleared before it compiles, so it must be one the caller names.
        if !parsed.kotlinc_arguments.is_empty()
            && !parsed
                .kotlinc_arguments
                .iter()
                .any(|argument| argument == "-d")
        {
            return Err("the kotlinc arguments name krusty's own output with -d".to_string());
        }
        Ok(parsed)
    }
}

/// `krusty verify-report [options] <record or directory of records>...`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VerifyReportArguments {
    /// The directory the reports are written to.
    pub out: PathBuf,
    /// The build tool and its version, such as `Gradle 9.1.0`.
    pub build_tool: Option<String>,
    /// The JDK the build ran on.
    pub jdk: Option<String>,
    /// Name compilations, classes and members in the issue text instead of anonymizing them.
    pub share_names: bool,
    /// Record files, or directories searched for `*.json` records.
    pub records: Vec<PathBuf>,
}

pub const VERIFY_REPORT_USAGE: &str = "usage: krusty verify-report --out <directory> \
[--build-tool <name and version>] [--jdk <version>] [--share-names] <record or directory>...";

impl VerifyReportArguments {
    pub fn parse(argv: &[String]) -> Result<VerifyReportArguments, String> {
        let mut parsed = VerifyReportArguments::default();
        let mut out = None;
        let mut arguments = argv.iter();
        while let Some(argument) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{argument} needs a value"))
            };
            match argument.as_str() {
                "--out" => out = Some(PathBuf::from(value()?)),
                "--build-tool" => parsed.build_tool = Some(value()?),
                "--jdk" => parsed.jdk = Some(value()?),
                "--share-names" => parsed.share_names = true,
                option if option.starts_with("--") => {
                    return Err(format!("unknown option '{option}'"))
                }
                record => parsed.records.push(PathBuf::from(record)),
            }
        }
        parsed.out = out.ok_or("--out is required")?;
        if parsed.records.is_empty() {
            return Err("no record to report".to_string());
        }
        Ok(parsed)
    }

    /// Every record the arguments name, in path order within each named directory.
    pub fn record_files(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for path in &self.records {
            if path.is_dir() {
                let mut found = std::fs::read_dir(path)?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()?;
                found.retain(|file| {
                    file.extension()
                        .is_some_and(|extension| extension == "json")
                });
                found.sort();
                files.extend(found);
            } else {
                files.push(path.clone());
            }
        }
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn verify_compile_takes_its_options_then_the_kotlinc_arguments() {
        let parsed = VerifyCompileArguments::parse(&strings(&[
            "--reference",
            "kotlinc",
            "--record",
            "r.json",
            "--compilation",
            ":compileKotlin",
            "--source-set",
            "main",
            "--kotlinc-millis",
            "1200",
            "--",
            "A.kt",
            "-d",
            "krusty",
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            VerifyCompileArguments {
                reference: PathBuf::from("kotlinc"),
                record: PathBuf::from("r.json"),
                compilation: ":compileKotlin".to_string(),
                source_set: Some("main".to_string()),
                kotlin_version: None,
                kotlinc_millis: Some(1200),
                unsupported: vec![],
                kotlinc_arguments: strings(&["A.kt", "-d", "krusty"]),
            }
        );
    }

    #[test]
    fn verify_compile_rejects_a_request_without_arguments_or_reasons() {
        assert_eq!(
            VerifyCompileArguments::parse(&strings(&[
                "--reference",
                "k",
                "--record",
                "r",
                "--compilation",
                "c",
            ])),
            Err("the kotlinc arguments follow '--'".to_string())
        );
        assert_eq!(
            VerifyCompileArguments::parse(&strings(&["--kotlinc-millis", "soon"])),
            Err("--kotlinc-millis takes milliseconds, not 'soon'".to_string())
        );
    }

    #[test]
    fn verify_compile_reads_unsupported_reasons_from_a_file() {
        let file = std::env::temp_dir().join(format!("krusty-refusals-{}.txt", std::process::id()));
        std::fs::write(&file, "krusty: plugin 'x' — no\n\nkrusty: -Xy\n").unwrap();
        let parsed = VerifyCompileArguments::parse(&strings(&[
            "--reference",
            "k",
            "--record",
            "r",
            "--compilation",
            "c",
            "--unsupported-from",
            file.to_str().unwrap(),
        ]))
        .unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            parsed.unsupported,
            strings(&["krusty: plugin 'x' — no", "krusty: -Xy"])
        );
    }

    #[test]
    fn verify_report_collects_records_and_options() {
        assert_eq!(
            VerifyReportArguments::parse(&strings(&[
                "--out",
                "report",
                "--share-names",
                "--build-tool",
                "Gradle 9.1.0",
                "records",
            ]))
            .unwrap(),
            VerifyReportArguments {
                out: PathBuf::from("report"),
                build_tool: Some("Gradle 9.1.0".to_string()),
                jdk: None,
                share_names: true,
                records: vec![PathBuf::from("records")],
            }
        );
    }

    #[test]
    fn a_record_round_trips_through_json() {
        let record = CompilationRecord {
            compilation: ":app:compileKotlin".to_string(),
            source_set: Some("main".to_string()),
            kotlin_version: Some("2.4.20".to_string()),
            outcome: Outcome::Unsupported {
                reasons: vec!["krusty: unsupported compiler plugin 'x'".to_string()],
            },
            kotlinc_millis: Some(10),
            krusty_millis: None,
            krusty_peak_rss_bytes: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(
            serde_json::from_str::<CompilationRecord>(&json).unwrap(),
            record
        );
    }
}
