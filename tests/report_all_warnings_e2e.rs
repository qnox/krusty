//! `-Xreport-all-warnings`: a compilation that fails on an error reports its warnings beside the
//! error, as kotlinc does. Without it, a failed compilation reports its errors only.

use std::fs;
use std::process::Command;

use super::common;

const SOURCE: &str = "fun f() = 1\nprivate fun g(): String = 1\n";

/// Remove only kotlinc's source excerpts, which krusty's plain renderer does not print. What remains
/// is the complete ordered diagnostic ledger.
fn diagnostic_ledger(stderr: &[u8]) -> Vec<String> {
    std::str::from_utf8(stderr)
        .expect("compiler diagnostics are UTF-8")
        .lines()
        .filter(|line| {
            !matches!(
                *line,
                "fun f() = 1"
                    | "private fun g(): String = 1"
                    | "^^^^^"
                    | "    ^"
                    | "                          ^"
            )
        })
        .map(str::to_string)
        .collect()
}

#[test]
fn a_failed_compilation_reports_its_warnings_under_report_all_warnings() {
    struct Case {
        tag: &'static str,
        arguments: &'static [&'static str],
        diagnostics: Vec<String>,
    }

    let krusty = common::krusty_binary();
    let dir =
        std::env::temp_dir().join(format!("krusty_report_all_warnings_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // Recorded kotlinc diagnostics retain the source path, so keep it stable across processes.
    let source = std::path::Path::new("target/test-sources/report-all-warnings/F.kt");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(source, SOURCE).expect("write source");
    let location = source.display();
    let visibility_warning =
        format!("{location}:1:1: warning: visibility must be specified in explicit API mode.");
    let return_warning =
        format!("{location}:1:5: warning: return type must be specified in explicit API mode.");
    let error =
        format!("{location}:2:27: error: return type mismatch: expected 'String', actual 'Int'.");
    let module_warning = "warning: opt-in requirement marker 'does.not.Exist' is unresolved. Make sure it's present in the module dependencies.".to_string();
    let cases = vec![
        Case {
            tag: "errors-only-by-default",
            arguments: &["-Xexplicit-api=warning", "-opt-in=does.not.Exist"],
            diagnostics: vec![error.clone()],
        },
        Case {
            tag: "warnings-beside-errors",
            arguments: &["-Xexplicit-api=warning", "-Xreport-all-warnings"],
            diagnostics: vec![
                visibility_warning.clone(),
                return_warning.clone(),
                error.clone(),
            ],
        },
        Case {
            tag: "module-warnings-beside-errors",
            arguments: &[
                "-Xexplicit-api=warning",
                "-opt-in=does.not.Exist",
                "-Xreport-all-warnings",
            ],
            diagnostics: vec![
                module_warning,
                visibility_warning.clone(),
                return_warning.clone(),
                error.clone(),
            ],
        },
        Case {
            tag: "no-werror-summary-beside-errors",
            arguments: &["-Xexplicit-api=warning", "-Xreport-all-warnings", "-Werror"],
            diagnostics: vec![visibility_warning, return_warning, error.clone()],
        },
        Case {
            tag: "nowarn-still-suppresses",
            arguments: &["-Xexplicit-api=warning", "-Xreport-all-warnings", "-nowarn"],
            diagnostics: vec![error],
        },
    ];

    for case in cases {
        let reference_out = dir.join(format!("{}-reference", case.tag));
        let mut reference_args = case
            .arguments
            .iter()
            .map(|argument| (*argument).to_string())
            .collect::<Vec<_>>();
        reference_args.extend([
            "-no-reflect".to_string(),
            "-d".to_string(),
            reference_out.display().to_string(),
            source.display().to_string(),
        ]);
        let (reference_code, reference_stderr) =
            common::byte_dump::with_recorded_diagnostics(|| {
                common::kotlinc_compile(&reference_args).expect("reference compiler unavailable")
            });
        assert_eq!(reference_code, 1, "{}", case.tag);
        assert_eq!(
            diagnostic_ledger(reference_stderr.as_bytes()),
            case.diagnostics,
            "{}: reference diagnostics",
            case.tag
        );

        let krusty_out = dir.join(format!("{}-krusty", case.tag));
        let result = Command::new(&krusty)
            .args(case.arguments)
            .args(["-no-reflect", "-d"])
            .arg(&krusty_out)
            .arg(source)
            .output()
            .expect("run krusty");
        assert_eq!(result.status.code(), Some(1), "{}", case.tag);
        let mut expected = case.diagnostics.clone();
        expected.push("krusty: 1 error(s)".to_string());
        assert_eq!(
            diagnostic_ledger(&result.stderr),
            expected,
            "{}: krusty diagnostics",
            case.tag
        );
        assert_eq!(result.stdout, b"", "{}", case.tag);
        assert!(!krusty_out.exists(), "{}: krusty wrote output", case.tag);
    }
    let _ = fs::remove_dir_all(&dir);
}
