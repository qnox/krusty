//! `-Xrender-internal-diagnostic-names`: kotlinc prints each named diagnostic as `[NAME] message`,
//! keeping the message's own capital, and leaves an unnamed one as it renders without the flag.

use std::fs;
use std::process::Command;

use super::common;

const SOURCE: &str = "fun f() = 1\n";

/// Remove only kotlinc's source excerpts, which krusty's plain renderer does not print. What remains
/// is the complete ordered diagnostic ledger.
fn diagnostic_ledger(stderr: &[u8]) -> Vec<String> {
    std::str::from_utf8(stderr)
        .expect("compiler diagnostics are UTF-8")
        .lines()
        .filter(|line| *line != "fun f() = 1" && !line.chars().all(|c| c == ' ' || c == '^'))
        .map(str::to_string)
        .collect()
}

#[test]
fn named_diagnostics_render_their_names_like_kotlinc() {
    struct Case {
        tag: &'static str,
        arguments: &'static [&'static str],
        code: i32,
        diagnostics: Vec<String>,
        /// What krusty prints after kotlinc's ledger.
        krusty_summary: Option<&'static str>,
    }

    let krusty = common::krusty_binary();
    let dir = std::env::temp_dir().join(format!(
        "krusty_render_diagnostic_names_{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // Recorded kotlinc diagnostics retain the source path, so keep it stable across processes.
    let source = std::path::Path::new("target/test-sources/render-internal-diagnostic-names/F.kt");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(source, SOURCE).expect("write source");
    let location = source.display();
    let named_visibility = format!(
        "{location}:1:1: warning: [NO_EXPLICIT_VISIBILITY_IN_API_MODE_WARNING] Visibility must be specified in explicit API mode."
    );
    let named_return = format!(
        "{location}:1:5: warning: [NO_EXPLICIT_RETURN_TYPE_IN_API_MODE_WARNING] Return type must be specified in explicit API mode."
    );
    let cases = vec![
        Case {
            tag: "names-hidden-by-default",
            arguments: &["-Xexplicit-api=warning"],
            code: 0,
            diagnostics: vec![
                format!("{location}:1:1: warning: visibility must be specified in explicit API mode."),
                format!("{location}:1:5: warning: return type must be specified in explicit API mode."),
            ],
            krusty_summary: None,
        },
        Case {
            tag: "named-source-warnings",
            arguments: &["-Xexplicit-api=warning", "-Xrender-internal-diagnostic-names"],
            code: 0,
            diagnostics: vec![named_visibility.clone(), named_return.clone()],
            krusty_summary: None,
        },
        Case {
            tag: "named-source-errors",
            arguments: &["-Xexplicit-api=strict", "-Xrender-internal-diagnostic-names"],
            code: 1,
            diagnostics: vec![
                format!("{location}:1:1: error: [NO_EXPLICIT_VISIBILITY_IN_API_MODE] Visibility must be specified in explicit API mode."),
                format!("{location}:1:5: error: [NO_EXPLICIT_RETURN_TYPE_IN_API_MODE] Return type must be specified in explicit API mode."),
            ],
            krusty_summary: Some("krusty: 2 error(s)"),
        },
        Case {
            tag: "named-module-warning",
            arguments: &[
                "-Xexplicit-api=warning",
                "-opt-in=does.not.Exist",
                "-Xrender-internal-diagnostic-names",
            ],
            code: 0,
            diagnostics: vec![
                "warning: [OPT_IN_REQUIREMENT_MARKER_IS_UNRESOLVED] Opt-in requirement marker 'does.not.Exist' is unresolved. Make sure it's present in the module dependencies.".to_string(),
                named_visibility.clone(),
                named_return.clone(),
            ],
            krusty_summary: None,
        },
        Case {
            tag: "named-command-line-warning",
            arguments: &[
                "-Xexplicit-api=warning",
                "-Xcontext-parameters",
                "-Xrender-internal-diagnostic-names",
            ],
            code: 0,
            diagnostics: vec![
                "warning: [REDUNDANT_CLI_ARG] The argument '-Xcontext-parameters' is redundant for the current language version 2.4.".to_string(),
                named_visibility.clone(),
                named_return.clone(),
            ],
            krusty_summary: None,
        },
        Case {
            tag: "werror-summary-stays-unnamed",
            arguments: &[
                "-Xexplicit-api=warning",
                "-Werror",
                "-Xrender-internal-diagnostic-names",
            ],
            code: 1,
            diagnostics: vec![
                "error: warnings found and -Werror specified".to_string(),
                named_visibility,
                named_return,
            ],
            krusty_summary: None,
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
        assert_eq!(reference_code, case.code, "{}", case.tag);
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
        assert_eq!(result.status.code(), Some(case.code), "{}", case.tag);
        let mut expected = case.diagnostics.clone();
        expected.extend(case.krusty_summary.map(str::to_string));
        assert_eq!(
            diagnostic_ledger(&result.stderr),
            expected,
            "{}: krusty diagnostics",
            case.tag
        );
        let stdout = if case.code == 0 {
            format!("ok: emitted 1 class file(s) to {}\n", krusty_out.display())
        } else {
            String::new()
        };
        assert_eq!(
            String::from_utf8_lossy(&result.stdout),
            stdout,
            "{}",
            case.tag
        );
    }
    let _ = fs::remove_dir_all(&dir);
}
