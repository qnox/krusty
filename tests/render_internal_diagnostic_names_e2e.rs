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
        // Every position krusty reports an unresolved name from: an import, an annotation, a
        // supertype, a declared type, a read, a call, a member read, call and write, a callable
        // reference and an operator convention.
        Case {
            file: "Unresolved.kt",
            source: "\
import missing.Thing

@Absent
class C : Base()

class Box { var v = 1 }

fun f(b: Box, s: String, n: Number?): Missing {
    val a = unknown
    nothing()
    b.absent
    b.absent()
    b.gone = 2
    val r = ::lost
    var m = s
    m++
    return a
}
",
            arguments: &[],
            diagnostics: &[
                "Unresolved.kt:1:8: error: [UNRESOLVED_IMPORT] Unresolved reference 'missing'.",
                "Unresolved.kt:3:2: error: [UNRESOLVED_REFERENCE] Unresolved reference 'Absent'.",
                "Unresolved.kt:4:11: error: [UNRESOLVED_REFERENCE] Unresolved reference 'Base'.",
                "Unresolved.kt:8:39: error: [UNRESOLVED_REFERENCE] Unresolved reference 'Missing'.",
                "Unresolved.kt:9:13: error: [UNRESOLVED_REFERENCE] Unresolved reference 'unknown'.",
                "Unresolved.kt:10:5: error: [UNRESOLVED_REFERENCE] Unresolved reference 'nothing'.",
                "Unresolved.kt:11:7: error: [UNRESOLVED_REFERENCE] Unresolved reference 'absent' on receiver of type 'Box'.",
                "Unresolved.kt:12:7: error: [UNRESOLVED_REFERENCE] Unresolved reference 'absent' on receiver of type 'Box'.",
                "Unresolved.kt:13:7: error: [UNRESOLVED_REFERENCE] Unresolved reference 'gone' on receiver of type 'Box'.",
                "Unresolved.kt:14:15: error: [UNRESOLVED_REFERENCE] Unresolved reference 'lost'.",
                "Unresolved.kt:16:6: error: [UNRESOLVED_REFERENCE] Unresolved reference 'inc' for operator '++' on receiver of type 'String'.",
            ],
            errors: 11,
        },
        // Every expected-type position krusty checks an expression against.
        Case {
            file: "Mismatch.kt",
            source: "\
fun take(i: Int) {}
fun r(): Int = \"s\"
fun g(): Int { return \"s\" }
val p: Int = \"s\"
var q = 1
fun f() {
    q = \"s\"
    take(\"s\")
    if (\"s\") {}
    take(null)
}
",
            arguments: &[],
            diagnostics: &[
                "Mismatch.kt:2:16: error: [RETURN_TYPE_MISMATCH] Return type mismatch: expected 'Int', actual 'String'.",
                "Mismatch.kt:3:23: error: [RETURN_TYPE_MISMATCH] Return type mismatch: expected 'Int', actual 'String'.",
                "Mismatch.kt:4:12: error: [INITIALIZER_TYPE_MISMATCH] Initializer type mismatch: expected 'Int', actual 'String'.",
                "Mismatch.kt:7:7: error: [ASSIGNMENT_TYPE_MISMATCH] Assignment type mismatch: actual type is 'String', but 'Int' was expected.",
                "Mismatch.kt:8:10: error: [ARGUMENT_TYPE_MISMATCH] Argument type mismatch: actual type is 'String', but 'Int' was expected.",
                "Mismatch.kt:9:9: error: [CONDITION_TYPE_MISMATCH] Condition type mismatch: inferred type is 'String' but 'Boolean' was expected.",
                "Mismatch.kt:10:10: error: [NULL_FOR_NONNULL_TYPE] Null cannot be a value of a non-null type 'Int'.",
            ],
            errors: 7,
        },
        // Every value-argument form kotlinc reports as an argument, and the do-while condition.
        Case {
            file: "ArgumentPositions.kt",
            source: "\
annotation class Ann(val x: Int)
@Ann(\"s\") fun ann() {}
fun <T : Number> one(t: T) {}
fun f() {
    one<Int>(\"s\")
    do {} while (\"s\")
}
",
            arguments: &[],
            diagnostics: &[
                "ArgumentPositions.kt:2:6: error: [ARGUMENT_TYPE_MISMATCH] Argument type mismatch: actual type is 'String', but 'Int' was expected.",
                "ArgumentPositions.kt:5:14: error: [ARGUMENT_TYPE_MISMATCH] Argument type mismatch: actual type is 'String', but 'Int' was expected.",
                "ArgumentPositions.kt:6:18: error: [CONDITION_TYPE_MISMATCH] Condition type mismatch: inferred type is 'String' but 'Boolean' was expected.",
            ],
            errors: 3,
        },
        // The other positions krusty names, each pinned by kotlinc's report: delegation, getter,
        // local and lambda bodies, local and default-value initializers, every condition form and
        // the null literal in each family.
        Case {
            file: "MismatchPositions.kt",
            source: "\
interface I
class D(val s: String) : I by s
val get: Int get() = \"s\"
val nul: Int get() = null
var q = 1
fun f(b: Boolean, a: Any) {
    val v: Int = \"s\"
    val w: Int = null
    q = null
    if (\"s\" && b) {}
    while (\"s\") {}
    when { \"s\" -> {} }
    if (null) {}
    val l: () -> Int = { \"s\" }
    when (a) { is String if \"s\" -> {} else -> {} }
    fun loc(): Int = \"s\"
    fun def(p: Int = \"s\") {}
}
",
            arguments: &[],
            diagnostics: &[
                "MismatchPositions.kt:2:31: error: [TYPE_MISMATCH] Type mismatch: inferred type is 'String', but 'I' was expected.",
                "MismatchPositions.kt:3:22: error: [RETURN_TYPE_MISMATCH] Return type mismatch: expected 'Int', actual 'String'.",
                "MismatchPositions.kt:4:22: error: [NULL_FOR_NONNULL_TYPE] Null cannot be a value of a non-null type 'Int'.",
                "MismatchPositions.kt:7:16: error: [INITIALIZER_TYPE_MISMATCH] Initializer type mismatch: expected 'Int', actual 'String'.",
                "MismatchPositions.kt:8:18: error: [NULL_FOR_NONNULL_TYPE] Null cannot be a value of a non-null type 'Int'.",
                "MismatchPositions.kt:9:9: error: [NULL_FOR_NONNULL_TYPE] Null cannot be a value of a non-null type 'Int'.",
                "MismatchPositions.kt:10:9: error: [CONDITION_TYPE_MISMATCH] Condition type mismatch: inferred type is 'String' but 'Boolean' was expected.",
                "MismatchPositions.kt:11:12: error: [CONDITION_TYPE_MISMATCH] Condition type mismatch: inferred type is 'String' but 'Boolean' was expected.",
                "MismatchPositions.kt:12:12: error: [CONDITION_TYPE_MISMATCH] Condition type mismatch: inferred type is 'String' but 'Boolean' was expected.",
                "MismatchPositions.kt:13:9: error: [CONDITION_TYPE_MISMATCH] Condition type mismatch: inferred type is 'Nothing?' but 'Boolean' was expected.",
                "MismatchPositions.kt:14:26: error: [RETURN_TYPE_MISMATCH] Return type mismatch: expected 'Int', actual 'String'.",
                "MismatchPositions.kt:15:29: error: [CONDITION_TYPE_MISMATCH] Condition type mismatch: inferred type is 'String' but 'Boolean' was expected.",
                "MismatchPositions.kt:16:22: error: [RETURN_TYPE_MISMATCH] Return type mismatch: expected 'Int', actual 'String'.",
                "MismatchPositions.kt:17:20: error: [INITIALIZER_TYPE_MISMATCH] Initializer type mismatch: expected 'Int', actual 'String'.",
            ],
            errors: 14,
        },
        // A construct whose language feature the compilation does not enable: one a later
        // language version enables, and an experimental one.
        Case {
            file: "Unsupported.kt",
            source: "\
fun f(): Int {
    val [a, b] = listOf(1, 2)
    return a + b
}
value class V(val a: Int, val b: Int)
",
            arguments: &[],
            diagnostics: &[
                "Unsupported.kt:2:9: error: [UNSUPPORTED_FEATURE] The feature \"name based destructuring\" is only available since language version 2.5",
                "Unsupported.kt:5:1: error: [UNSUPPORTED_FEATURE] The feature \"full value classes\" is experimental and should be enabled explicitly. This can be done by supplying the compiler argument '-XXLanguage:+FullValueClasses', but note that no stability guarantees are provided.",
            ],
            errors: 2,
        },
        // A context-parameter clause under a language version before the feature's.
        Case {
            file: "ContextOff.kt",
            source: "\
context(a: String)
fun f(): String = a
",
            arguments: &["-language-version", "2.1"],
            diagnostics: &[
                "warning: [DEPRECATED_LANGUAGE_VERSION] Language version 2.1 is deprecated and its support will be removed in a future version of Kotlin. Update the version to 2.2.",
                "ContextOff.kt:1:1: error: [UNSUPPORTED_FEATURE] The feature \"context parameters\" is only available since language version 2.4",
            ],
            errors: 1,
        },
        // Checker errors that kotlinc reports under their own names.
        Case {
            file: "Checks.kt",
            source: "\
annotation class A(val x: Int)
var n = 1
@A(n) fun a() {}
fun b(s: String?) = s.length
inline fun c(f: () -> Unit) { val g = f; g() }
class In<in T>(val t: T)
fun d() { val v = 1; v = 2 }
fun e(s: String?) = s.trim()
",
            arguments: &[],
            diagnostics: &[
                "Checks.kt:3:4: error: [ANNOTATION_ARGUMENT_MUST_BE_CONST] Annotation argument must be a compile-time constant.",
                "Checks.kt:4:22: error: [UNSAFE_CALL] Only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
                "Checks.kt:5:39: error: [USAGE_IS_NOT_INLINABLE] Illegal usage of inline parameter 'f: () -> Unit'. Add 'noinline' modifier to the parameter declaration.",
                "Checks.kt:6:23: error: [TYPE_VARIANCE_CONFLICT_ERROR] Type parameter 'T' is declared as 'in' but occurs in 'out' position in type 'T (of class In<in T>)'.",
                "Checks.kt:7:22: error: [VAL_REASSIGNMENT] 'val' cannot be reassigned.",
                "Checks.kt:8:22: error: [UNSAFE_CALL] Only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
            ],
            errors: 6,
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
