//! `-Xrender-internal-diagnostic-names`: kotlinc prints each named diagnostic as `[NAME] message`,
//! keeping the message's own capital, and leaves an unnamed one as it renders without the flag.

use std::fs;
use std::process::Command;

use super::common;

const SOURCE: &str = "fun f() = 1\n";

/// Remove only kotlinc's source excerpts, which krusty's plain renderer does not print. What remains
/// is the complete ordered diagnostic ledger.
fn diagnostic_ledger(stderr: &[u8]) -> Vec<String> {
    source_ledger(stderr, SOURCE)
}

/// [`diagnostic_ledger`] for a compilation of `source`.
fn source_ledger(stderr: &[u8], source: &str) -> Vec<String> {
    std::str::from_utf8(stderr)
        .expect("compiler diagnostics are UTF-8")
        .lines()
        .filter(|line| {
            !source.lines().any(|source_line| source_line == *line)
                && !line.chars().all(|c| c == ' ' || c == '^')
        })
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
                named_visibility.clone(),
                named_return.clone(),
            ],
            krusty_summary: None,
        },
        Case {
            tag: "named-level-disables-a-source-warning",
            arguments: &[
                "-Xexplicit-api=warning",
                "-Xwarning-level=NO_EXPLICIT_VISIBILITY_IN_API_MODE_WARNING:disabled",
                "-Xrender-internal-diagnostic-names",
            ],
            code: 0,
            diagnostics: vec![named_return],
            krusty_summary: None,
        },
        Case {
            tag: "named-level-promotes-the-module-warning",
            arguments: &[
                "-Xexplicit-api=warning",
                "-opt-in=does.not.Exist",
                "-Xwarning-level=OPT_IN_REQUIREMENT_MARKER_IS_UNRESOLVED:error",
                "-Xrender-internal-diagnostic-names",
            ],
            code: 1,
            diagnostics: vec![
                "error: [OPT_IN_REQUIREMENT_MARKER_IS_UNRESOLVED] Opt-in requirement marker 'does.not.Exist' is unresolved. Make sure it's present in the module dependencies.".to_string(),
            ],
            krusty_summary: Some("krusty: 1 error(s)"),
        },
        Case {
            tag: "no-level-reaches-the-pre-release-warning",
            arguments: &[
                "-XXLanguage:+CompanionBlocksAndExtensions",
                "-Xwarning-level=COMPILER_ARGUMENTS_WARNING:disabled",
                "-Xrender-internal-diagnostic-names",
            ],
            code: 0,
            diagnostics: [
                "warning: ATTENTION!",
                "This build uses unsafe internal compiler arguments:",
                "-XXLanguage:+CompanionBlocksAndExtensions",
                "This mode is not recommended for production use,",
                "as no stability/compatibility guarantees are given on",
                "compiler or generated code. Use it at your own risk!",
                "warning: [COMPILER_ARGUMENTS_WARNING] Following manually enabled features will force generation of pre-release binaries: CompanionBlocksAndExtensions",
            ]
            .map(str::to_string)
            .to_vec(),
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

/// Errors keep kotlinc's names: the deprecation-cycle errors once their language feature forbids
/// the construct, and the reified, class-literal, qualifier and value-class errors.
#[test]
fn named_errors_render_their_names_like_kotlinc() {
    struct Case {
        file: &'static str,
        source: &'static str,
        arguments: &'static [&'static str],
        diagnostics: &'static [&'static str],
        /// krusty's error count.
        errors: usize,
    }

    let cases = [
        Case {
            file: "Errors.kt",
            source: "\
typealias L<T> = List<T>
fun l() = L<Int>::class
@JvmInline value class V1(var x: Int)
@JvmInline value class V2(val x: Nothing)
@JvmInline value class V3(val x: V3)
object O
fun o() = O<Int>
inline fun <reified T> r(): String = T::class.java.name
fun <T> f1() = r<T>()
fun <T> f2() = r<Array<T>>()
fun f3() = r<Nothing>()
fun <T> f4() = r<T & Any>()
interface I1
interface I2
class C12 : I1, I2
class D12 : I1, I2
inline fun <reified T> ra(x: T): T = x
fun f5(b: Boolean) = ra(if (b) C12() else D12())
",
            arguments: &[],
            diagnostics: &[
                "Errors.kt:2:11: error: [CLASS_LITERAL_LHS_NOT_A_CLASS] Only classes are allowed on the left-hand side of a class literal.",
                "Errors.kt:3:27: error: [VALUE_CLASS_CONSTRUCTOR_NOT_FINAL_READ_ONLY_PARAMETER] Value class primary constructor must only have final read-only ('val') property parameters.",
                "Errors.kt:4:34: error: [VALUE_CLASS_HAS_INAPPLICABLE_PARAMETER_TYPE] Value class cannot have value parameter of type 'kotlin/Nothing'.",
                "Errors.kt:5:34: error: [VALUE_CLASS_CANNOT_BE_RECURSIVE] Value class cannot be recursive.",
                "Errors.kt:7:12: error: [EXPLICIT_TYPE_ARGUMENTS_IN_PROPERTY_ACCESS] Object access cannot have explicit type arguments.",
                "Errors.kt:9:18: error: [TYPE_PARAMETER_AS_REIFIED] Cannot use 'T' as reified type parameter. Use a class instead.",
                "Errors.kt:10:18: error: [TYPE_PARAMETER_AS_REIFIED_ARRAY_ERROR] Cannot use 'T' as reified type parameter, since the array type parameter is not reified.",
                "Errors.kt:11:14: error: [REIFIED_TYPE_FORBIDDEN_SUBSTITUTION] Cannot use 'Nothing' as reified type parameter.",
                "Errors.kt:12:18: error: [DEFINITELY_NON_NULLABLE_AS_REIFIED] Cannot use definitely-non-nullable type as a reified type argument.",
                "Errors.kt:18:22: error: [TYPE_INTERSECTION_AS_REIFIED_ERROR] Type argument for reified type parameter 'T' was inferred to the intersection of ['I1' & 'I2']. Reification of an intersection type results in the common supertype being used. This may lead to subtle issues and an explicit type argument is encouraged.",
            ],
            errors: 10,
        },
        Case {
            file: "Forbidden.kt",
            source: "\
typealias Yes<T> = String
typealias Two<T> = String
fun c1() = Yes<Int>::class
fun c2() = Two<Int, Int>::class
inline fun <reified T> c3() = T<Int>::class
fun s() = java.lang.Integer<Int>.MAX_VALUE
fun p() = (kotlin).collections.listOf(1)
",
            arguments: &["-language-version", "2.5"],
            diagnostics: &[
                "warning: [EXPERIMENTAL_LANGUAGE_VERSION] Language version 2.5 is experimental, there are no backwards compatibility guarantees for new language and library features. Use the stable version 2.4 instead.",
                "Forbidden.kt:3:12: error: [CLASS_LITERAL_LHS_NOT_A_CLASS] Only classes are allowed on the left-hand side of a class literal.",
                "Forbidden.kt:4:15: error: [WRONG_NUMBER_OF_TYPE_ARGUMENTS] One type argument expected for 'typealias Two<T> = String'.",
                "Forbidden.kt:5:32: error: [TYPE_ARGUMENTS_NOT_ALLOWED] Type arguments are not allowed for type parameters.",
                "Forbidden.kt:6:28: error: [TYPE_ARGUMENTS_NOT_ALLOWED] Type arguments are not allowed when static member is accessed.",
                "Forbidden.kt:7:11: error: [PARENTHESIZED_PACKAGE_QUALIFIER_ERROR] Wrapping package qualifiers in parentheses is not allowed.",
            ],
            errors: 5,
        },
    ];

    let krusty = common::krusty_binary();
    let dir =
        std::env::temp_dir().join(format!("krusty_render_error_names_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for case in cases {
        // Recorded kotlinc diagnostics retain the source path, so keep it stable across processes.
        let directory =
            std::path::Path::new("target/test-sources/render-internal-diagnostic-names");
        fs::create_dir_all(directory).unwrap();
        let source = directory.join(case.file);
        fs::write(&source, case.source).expect("write source");
        let location = source.display().to_string();
        let expected = case
            .diagnostics
            .iter()
            .map(|line| line.replacen(case.file, &location, 1))
            .collect::<Vec<_>>();

        let reference_out = dir.join(format!("{}-reference", case.file));
        let mut reference_args = case
            .arguments
            .iter()
            .map(|argument| (*argument).to_string())
            .collect::<Vec<_>>();
        reference_args.extend([
            "-Xrender-internal-diagnostic-names".to_string(),
            "-no-reflect".to_string(),
            "-d".to_string(),
            reference_out.display().to_string(),
            location.clone(),
        ]);
        let (reference_code, reference_stderr) =
            common::byte_dump::with_recorded_diagnostics(|| {
                common::kotlinc_compile(&reference_args).expect("reference compiler unavailable")
            });
        assert_eq!(reference_code, 1, "{}", case.file);
        assert_eq!(
            source_ledger(reference_stderr.as_bytes(), case.source),
            expected,
            "{}: reference diagnostics",
            case.file
        );

        let krusty_out = dir.join(format!("{}-krusty", case.file));
        let result = Command::new(&krusty)
            .args(case.arguments)
            .args(["-Xrender-internal-diagnostic-names", "-no-reflect", "-d"])
            .arg(&krusty_out)
            .arg(&source)
            .output()
            .expect("run krusty");
        assert_eq!(result.status.code(), Some(1), "{}", case.file);
        let mut krusty_expected = expected.clone();
        krusty_expected.push(format!("krusty: {} error(s)", case.errors));
        assert_eq!(
            source_ledger(&result.stderr, case.source),
            krusty_expected,
            "{}: krusty diagnostics",
            case.file
        );
        assert_eq!(result.stdout, b"", "{}", case.file);
    }
    let _ = fs::remove_dir_all(&dir);
}
