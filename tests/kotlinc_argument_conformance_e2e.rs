//! krusty's command line against kotlinc's own parser.
//!
//! Two checks, both against the provisioned reference `kotlin-compiler.jar`:
//!
//! * the argument table krusty parses with (`crates/krusty-cli/src/kotlinc_arguments/releases`)
//!   is exactly what `scripts/kotlinc-arguments/DumpKotlincArguments.java` reads from the
//!   compiler's annotations, so no name, alias, type, delimiter, lifecycle or feature toggle drifts;
//! * for a corpus generated from that table (every argument, every spelling, every value form),
//!   krusty's tokenizer parses the same values and sources and reports the same syntax errors and
//!   warnings, in the same order and words, as kotlinc's `parseCommandLineArguments`,
//!   `validateArgumentsAllErrors` and `reportArgumentParseProblems`.

use std::fmt::Write as _;

use krusty::kotlin_version::KotlinVersion;
use krusty_cli::kotlinc_arguments::{catalog, tokenize, Catalog, Recorded, ValueKind};

use super::common;

const ORACLE: &str = include_str!("kotlinc_arguments/KotlincArgumentOracle.java");
const DUMPER: &str = include_str!("../scripts/kotlinc-arguments/DumpKotlincArguments.java");
const FEATURE_DUMPER: &str = include_str!("../scripts/kotlinc-arguments/DumpLanguageFeatures.java");
const VERSION_DUMPER: &str = include_str!("../scripts/kotlinc-arguments/DumpLanguageVersions.java");

/// The reference compiler's classpath: the compiler and the standard library it runs on.
fn compiler_classpath() -> String {
    let compiler = common::kotlin_compiler_jar().expect("the reference kotlin-compiler.jar");
    let stdlib = compiler.with_file_name("kotlin-stdlib.jar");
    format!("{}:{}", compiler.display(), stdlib.display())
}

/// Compile and run a Java program against the reference compiler; return its stdout.
fn run_java(name: &str, main_class: &str, source: &str) -> String {
    let dir = common::scratch_dir()
        .expect("a scratch directory")
        .join(format!("kotlinc-arguments-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let path = dir.join(format!("{main_class}.java"));
    std::fs::write(&path, source).expect("write the Java source");
    common::javac_run(
        path.to_str().unwrap(),
        &compiler_classpath(),
        dir.to_str().unwrap(),
        main_class,
    )
    .expect("the pooled Java runner")
}

/// The cases, one argument per line and an empty line between cases, run through kotlinc's parser.
/// Returns the reference release and its report.
fn kotlinc_report(cases: &[Vec<String>]) -> (KotlinVersion, String) {
    let dir = common::scratch_dir()
        .expect("a scratch directory")
        .join("kotlinc-arguments-cases");
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let cases_path = dir.join("cases.txt");
    let text: Vec<String> = cases.iter().map(|case| case.join("\n")).collect();
    std::fs::write(&cases_path, text.join("\n\n") + "\n\n").expect("write the cases");
    let source = ORACLE.replace("@CASES@", cases_path.to_str().unwrap());
    let output = run_java("oracle", "KotlincArgumentOracle", &source);
    let (header, report) = output.split_once('\n').expect("a version line");
    let version = header
        .strip_prefix("version\t")
        .and_then(KotlinVersion::parse)
        .unwrap_or_else(|| panic!("unexpected oracle header {header:?}"));
    (version, report.to_string())
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// krusty's tokenizer, reported in the oracle's format.
fn krusty_report(catalog: &Catalog, cases: &[Vec<String>]) -> String {
    let mut report = String::new();
    for case in cases {
        let tokenized = tokenize(catalog, case.clone(), Vec::new());
        report.push_str("case\n");
        for error in tokenized.errors() {
            writeln!(report, "error\t{}", escape(&error)).unwrap();
        }
        for warning in tokenized.warnings() {
            writeln!(report, "warning\t{}", escape(&warning)).unwrap();
        }
        for (spec, recorded) in &tokenized.explicit {
            write!(report, "explicit\t{}", spec.name).unwrap();
            match recorded {
                Recorded::Scalars(values) => {
                    for value in values {
                        write!(report, "\t{}", escape(value)).unwrap();
                    }
                }
                Recorded::List(values) => write!(report, "\t[{}]", values.join("|")).unwrap(),
            }
            report.push('\n');
        }
        for free in &tokenized.free {
            writeln!(report, "free\t{}", escape(free)).unwrap();
        }
    }
    report
}

/// Every spelling of every argument the release declares, in every value form kotlinc
/// distinguishes, plus the tokens no argument claims.
fn corpus(catalog: &Catalog) -> Vec<Vec<String>> {
    let mut cases: Vec<Vec<String>> = Vec::new();
    let case = |arguments: &[&str]| -> Vec<String> {
        std::iter::once("A.kt")
            .chain(arguments.iter().copied())
            .map(str::to_string)
            .collect()
    };
    for spec in catalog.arguments() {
        // `-XXLanguage` values are checked against kotlinc's language-feature table, which krusty
        // does not mirror; its syntax is covered by the tokenizer's unit tests.
        if spec.name == "-XXLanguage" {
            continue;
        }
        let legal = spec.legal_values();
        let value = legal.first().copied().unwrap_or("v1");
        let keys = [
            Some(&spec.name),
            spec.short_name.as_ref(),
            spec.deprecated_name.as_ref(),
        ];
        for key in keys.into_iter().flatten() {
            let key = key.as_str();
            match spec.kind {
                ValueKind::Bool => {
                    for form in ["", "=true", "=false", "=maybe"] {
                        cases.push(case(&[&format!("{key}{form}")]));
                    }
                    cases.push(case(&[key, key]));
                    cases.push(case(&[key, &format!("{key}=false")]));
                }
                ValueKind::String | ValueKind::Array => {
                    cases.push(case(&[&format!("{key}={value}")]));
                    cases.push(case(&[key, value]));
                    cases.push(case(&[key]));
                    cases.push(case(&[
                        &format!("{key}={value}"),
                        &format!("{key}={value}"),
                    ]));
                    cases.push(case(&[&format!("{key}={value}"), &format!("{key}=other")]));
                    cases.push(case(&[&format!("{key}=a,b")]));
                    cases.push(case(&[&format!("{key}=a:b")]));
                    if !legal.is_empty() {
                        cases.push(case(&[&format!("{key}=bogus")]));
                    }
                }
            }
        }
    }
    for edge in [
        &["-foo"][..],
        &["-Xfoo"],
        &["-Xfoo=1"],
        &["--", "-d", "-x.kt"],
        &["-d", "out", "--", "-Xfoo"],
        &["B.kt", "-d"],
        &["-d=out"],
        &["-XXfoo"],
        &["-"],
        &["-X"],
    ] {
        cases.push(case(edge));
    }
    cases
}

#[test]
fn the_argument_table_is_what_the_reference_compiler_declares() {
    let (version, _) = kotlinc_report(&[]);
    let dumped = run_java("dump", "DumpKotlincArguments", DUMPER);
    let vendored = catalog::vendored_table(version)
        .unwrap_or_else(|| panic!("kotlinc {version} has no vendored argument table"));
    assert_eq!(
        dumped, vendored,
        "kotlinc {version} declares a different argument table; regenerate it with \
         `just kotlinc-arguments {version}`"
    );
}

#[test]
fn the_language_feature_table_is_what_the_reference_compiler_declares() {
    let (version, _) = kotlinc_report(&[]);
    let dumped = run_java("features", "DumpLanguageFeatures", FEATURE_DUMPER);
    let vendored = krusty::features::vendored_table(version)
        .unwrap_or_else(|| panic!("kotlinc {version} has no vendored language feature table"));
    assert_eq!(
        dumped, vendored,
        "kotlinc {version} declares a different language feature table; regenerate it with \
         `just kotlinc-arguments {version}`"
    );
}

#[test]
fn the_language_version_policy_is_what_the_reference_compiler_declares() {
    let (version, _) = kotlinc_report(&[]);
    let dumped = run_java("versions", "DumpLanguageVersions", VERSION_DUMPER);
    let vendored = krusty::features::vendored_language_versions(version)
        .unwrap_or_else(|| panic!("kotlinc {version} has no vendored language version policy"));
    assert_eq!(
        dumped, vendored,
        "kotlinc {version} declares a different language version policy; regenerate it with \
         `just kotlinc-arguments {version}`"
    );
}

#[test]
fn the_parser_matches_kotlincs_on_every_argument_form() {
    let (version, _) = kotlinc_report(&[]);
    let catalog = Catalog::for_version(version)
        .unwrap_or_else(|| panic!("no krusty argument table for kotlinc {version}"));
    let cases = corpus(catalog);
    let (_, expected) = kotlinc_report(&cases);
    let actual = krusty_report(catalog, &cases);
    let expected_cases: Vec<&str> = expected.split("case\n").skip(1).collect();
    let actual_cases: Vec<&str> = actual.split("case\n").skip(1).collect();
    assert_eq!(actual_cases.len(), cases.len(), "krusty report");
    assert_eq!(expected_cases.len(), cases.len(), "kotlinc report");
    let mismatches: Vec<String> = cases
        .iter()
        .zip(expected_cases.iter().zip(&actual_cases))
        .filter(|(_, (expected, actual))| expected != actual)
        .map(|(case, (expected, actual))| {
            format!("{case:?}\n  kotlinc:\n{expected}  krusty:\n{actual}")
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} of {} cases differ from kotlinc {version}:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
}
