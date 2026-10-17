//! kotlinc's checks on a `data class` whose primary constructor is not public
//! (`FirDataClassNonPublicConstructorChecker` and `FirDataClassCopyUsageWillBecomeInaccessibleChecker`):
//! its generated `copy()` exposes that constructor. The declaration and each use of that `copy()`
//! where the constructor is not visible are deprecation warnings until
//! `ErrorAboutDataClassCopyVisibilityChange` makes them errors.

use super::common;

const FORBID: &str = "-XXLanguage:+ErrorAboutDataClassCopyVisibilityChange";

/// Compile `source` with both compilers, passing `args` to each, and require the same complete
/// errors and warnings, which must also be `errors` and `warnings`. A diagnostic's first line is
/// what both compilers' output is compared on.
fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[String], warnings: &[String]) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("DataClasses.kt", source)],
        &[common::stdlib_jar()],
        &args,
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty_errors = common::compiler_errors(&krusty_output);
    let kotlinc_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    let krusty_warnings = common::compiler_warnings(&krusty_output);
    let kotlinc_warnings = common::compiler_warnings(&result.reference_stderr);
    assert_eq!(
        krusty_warnings, kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    let render = |diagnostics: &[common::CompilerError]| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}: {}",
                    diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(render(&kotlinc_errors), errors);
    assert_eq!(render(&kotlinc_warnings), warnings);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

const DECLARATION: &str = "non-public primary constructor is exposed via the generated 'copy()' method of the 'data' class.\n\nThe generated 'copy()' will change its visibility in future releases.\n\nTo suppress the warning do one of the following:\n- Annotate the data class with the '@ConsistentCopyVisibility' annotation.\n- Use the '-Xconsistent-data-class-copy-visibility' compiler flag.\n- Annotate the data class with the '@ExposedCopyVisibility' annotation \n  (Discouraged, but can be used to keep binary compatibility).\n\nTo learn more, see the documentation of the '@ConsistentCopyVisibility' and '@ExposedCopyVisibility' annotations.\n\n";
const USAGE: &str = "this 'copy()' exposes the non-public primary constructor of a 'data class'. See the appropriate 'data class' documentation or contact the 'data class' author for migration guidance.";
const SUFFIX: &str =
    "This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KT-11914.";

const DATA_CLASSES: &str = "\
data class D private constructor(val x: Int)
@ExposedCopyVisibility
data class E private constructor(val x: Int)
@ConsistentCopyVisibility
data class F private constructor(val x: Int)
data class G internal constructor(val x: Int)
data class H protected constructor(val x: Int)
data object O
data class P public constructor(val x: Int)
data class Q<T> private constructor(val t: T)
@Suppress(\"DATA_CLASS_COPY_VISIBILITY_WILL_BE_CHANGED_WARNING\")
data class S private constructor(val x: Int)
fun use(d: D, e: E, g: G, q: Q<String>, s: S) {
    d.copy(x = 2)
    e.copy(x = 2)
    g.copy()
    q.copy(t = \"b\")
    val r = D::copy
    val r2 = d::copy
    s.copy()
    @Suppress(\"DATA_CLASS_INVISIBLE_COPY_USAGE_WARNING\") d.copy()
}
";

const DECLARATIONS: [&str; 4] = ["1:14", "6:14", "7:14", "10:17"];
const USES: [&str; 6] = ["14:7", "15:7", "17:7", "18:16", "19:17", "20:7"];

/// The first line of a diagnostic whose message is `message`.
fn first_line(position: &str, message: &str) -> String {
    format!("{position}: {}", message.lines().next().unwrap())
}

#[test]
fn exposed_data_class_copy_is_a_deprecation_warning() {
    let mut warnings = DECLARATIONS
        .iter()
        .map(|position| first_line(position, DECLARATION))
        .collect::<Vec<_>>();
    warnings.extend(
        USES.iter()
            .map(|position| format!("{position}: {USAGE} {SUFFIX}")),
    );
    assert_matches_kotlinc(DATA_CLASSES, &[], &[], &warnings);
    // The declaration message spans several lines; the front end reports it whole.
    let mut expected = DECLARATIONS
        .iter()
        .map(|position| format!("{position}: warning: {DECLARATION}{SUFFIX}"))
        .collect::<Vec<_>>();
    expected.extend(
        USES.iter()
            .map(|position| format!("{position}: warning: {USAGE} {SUFFIX}")),
    );
    let krusty = common::front_end_diagnostics_located(
        DATA_CLASSES,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(krusty, expected);
}

#[test]
fn forbidden_exposed_data_class_copy_is_an_error() {
    let mut errors = DECLARATIONS
        .iter()
        .chain(&["12:14"])
        .map(|position| first_line(position, DECLARATION))
        .collect::<Vec<_>>();
    errors.extend(
        USES.iter()
            .chain(&["21:60"])
            .map(|position| format!("{position}: {USAGE}")),
    );
    assert_matches_kotlinc(DATA_CLASSES, &[FORBID], &errors, &[]);
}

#[test]
fn user_declared_copy_overload_is_not_the_generated_copy() {
    let source = "\
@ExposedCopyVisibility
data class D private constructor(val x: Int) {
    fun copy(text: String) = text
}
fun use(d: D) = d.copy(\"ok\")
";
    assert_matches_kotlinc(source, &[], &[], &[]);
}
