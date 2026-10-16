//! kotlinc's `FirTypeArgumentsNotAllowedExpressionChecker` for a static member reached through a
//! classifier qualifier written with type arguments: a deprecation warning on the qualifier's
//! `<…>` list until `ForbidUselessTypeArgumentsIn25` makes it an error, and always an error for a
//! `companion { … }` block member or a companion extension.

use super::common;
use std::path::PathBuf;

const FORBID: &str = "-XXLanguage:+ForbidUselessTypeArgumentsIn25";
const COMPANION_BLOCKS: &str = "-XXLanguage:+CompanionBlocksAndExtensions";
const MESSAGE: &str = "type arguments are not allowed when static member is accessed.";

fn warning(location: &str) -> String {
    format!(
        "{location}: {MESSAGE} This will become an error in language version 2.5. See \
         https://youtrack.jetbrains.com/issue/KTLC-390."
    )
}

fn error(location: &str) -> String {
    format!("{location}: {MESSAGE}")
}

/// Compile `sources` with both compilers on `classpath`, passing `args` to each, and require the
/// same complete errors and warnings, which must also be `errors` and `warnings`.
fn assert_matches_kotlinc(
    sources: &[(&str, &str)],
    classpath: &[PathBuf],
    args: &[&str],
    errors: &[String],
    warnings: &[String],
) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(sources, classpath, &args);
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let render = |diagnostics: &[common::CompilerError]| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}:{}: {}",
                    diagnostic.file, diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    let kotlinc_errors = render(&common::compiler_errors(&result.reference_stderr));
    let kotlinc_warnings = render(&common::compiler_warnings(&result.reference_stderr));
    assert_eq!(
        kotlinc_errors, errors,
        "kotlinc\n{}",
        result.reference_stderr
    );
    assert_eq!(
        kotlinc_warnings, warnings,
        "kotlinc\n{}",
        result.reference_stderr
    );
    assert_eq!(
        render(&common::compiler_errors(&krusty_output)),
        kotlinc_errors,
        "errors\n{krusty_output}"
    );
    assert_eq!(
        render(&common::compiler_warnings(&krusty_output)),
        kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

const KOTLIN_STATICS: &str = "\
enum class E {
    X;
}

typealias ET = E

class K<TP> {
    fun interface I {
        fun foo()
    }
}

fun test() {
    E<Int>.X
    E<Int, String>.X
    E<Int>.values()
    E<Int, String>.valueOf(\"X\")
    ET<Int>.X
    ET<Int>.entries
    K<Int>.I { }
    K<*, *>.I { }
    E.X
    E.entries
    K.I { }
}

@Suppress(\"TYPE_ARGUMENTS_NOT_ALLOWED_WARNING\")
fun suppressed() {
    E<Int>.X
}
";

const KOTLIN_LOCATIONS: [&str; 8] = [
    "Statics.kt:14:6",
    "Statics.kt:15:6",
    "Statics.kt:16:6",
    "Statics.kt:17:6",
    "Statics.kt:18:7",
    "Statics.kt:19:7",
    "Statics.kt:20:6",
    "Statics.kt:21:6",
];

#[test]
fn kotlin_static_access_through_type_arguments_is_deprecated() {
    assert_matches_kotlinc(
        &[("Statics.kt", KOTLIN_STATICS)],
        &[common::stdlib_jar()],
        &[],
        &[],
        &KOTLIN_LOCATIONS.map(warning),
    );
}

#[test]
fn forbidden_kotlin_static_access_through_type_arguments_is_an_error() {
    let mut errors = KOTLIN_LOCATIONS.map(error).to_vec();
    errors.push(error("Statics.kt:29:6"));
    assert_matches_kotlinc(
        &[("Statics.kt", KOTLIN_STATICS)],
        &[common::stdlib_jar()],
        &[FORBID],
        &errors,
        &[],
    );
}

const JAVA_UTILS: &str = "\
public class JavaUtils<T> {
    public static void foo() { return; }
    public static String bar;
    public static final int CONST = 1;
    public static class Nested {
        public static void foo() { return; }
    }
    public interface I { void run(); }
}
";

const JAVA_STATICS: &str = "\
typealias JT<T> = JavaUtils<T>

fun test() {
    JavaUtils<String>.foo()
    JavaUtils<Nothing>.bar
    JavaUtils<Int>.CONST
    JavaUtils.Nested<Any?>.foo()
    JT<String>.foo()
    JT<Nothing>.bar
    JavaUtils<Int>.I { }
    JavaUtils.foo()
    JavaUtils.I { }
}
";

const JAVA_LOCATIONS: [&str; 7] = [
    "JavaStatics.kt:4:14",
    "JavaStatics.kt:5:14",
    "JavaStatics.kt:6:14",
    "JavaStatics.kt:7:21",
    "JavaStatics.kt:8:7",
    "JavaStatics.kt:9:7",
    "JavaStatics.kt:10:14",
];

fn java_classpath() -> Vec<PathBuf> {
    let (classes, _) = common::javac_compile(
        &[("JavaUtils.java".to_string(), JAVA_UTILS.to_string())],
        &[],
    )
    .expect("javac compiles the Java fixture");
    vec![common::stdlib_jar(), classes]
}

#[test]
fn java_static_access_through_type_arguments_is_deprecated() {
    assert_matches_kotlinc(
        &[("JavaStatics.kt", JAVA_STATICS)],
        &java_classpath(),
        &[],
        &[],
        &JAVA_LOCATIONS.map(warning),
    );
}

#[test]
fn forbidden_java_static_access_through_type_arguments_is_an_error() {
    assert_matches_kotlinc(
        &[("JavaStatics.kt", JAVA_STATICS)],
        &java_classpath(),
        &[FORBID],
        &JAVA_LOCATIONS.map(error),
        &[],
    );
}

const COMPANION_ASSOCIATED: &str = "\
class C<T> {
    companion {
        fun foo() = 1
        val bar = 2
        const val BAZ = 5
    }
}
companion fun C.ext() = 3
companion val C.prop: Int get() = 4

fun test() {
    C<Int>.foo()
    C<Int>.bar
    C<Int>.BAZ
    C<Int>.ext()
    C<Int>.prop
    C.foo()
    C.prop
}
";

const COMPANION_LOCATIONS: [&str; 5] = [
    "Companion.kt:12:6",
    "Companion.kt:13:6",
    "Companion.kt:14:6",
    "Companion.kt:15:6",
    "Companion.kt:16:6",
];

/// A companion-block member or companion extension never accepted type arguments on its qualifier,
/// so it is an error whether or not the deprecation is in force.
#[test]
fn companion_associated_access_through_type_arguments_is_an_error() {
    for args in [&[COMPANION_BLOCKS][..], &[COMPANION_BLOCKS, FORBID]] {
        assert_matches_kotlinc(
            &[("Companion.kt", COMPANION_ASSOCIATED)],
            &[common::stdlib_jar()],
            args,
            &COMPANION_LOCATIONS.map(error),
            &[],
        );
    }
}

/// A lexical value wins the first scope-tower segment. The later `.X` therefore belongs to the
/// selected value receiver; the homonymous classifier must not be reopened to manufacture a static
/// access diagnostic.
#[test]
fn a_local_value_shadows_the_classifier_qualifier_once() {
    let source = "\
class Holder(val X: Int)
class E<T> { companion object { val X = 2 } }

fun test() {
    val E = Holder(1)
    E<Int>.X
}
";
    assert_matches_kotlinc(
        &[("Shadow.kt", source)],
        &[common::stdlib_jar()],
        &[FORBID],
        &["Shadow.kt:6:6: property access cannot have explicit type arguments.".to_string()],
        &[],
    );
}

/// Kotlin metadata/classfiles normalize into the same classifier-member selection as live source;
/// the diagnostic must consume that selected identity rather than depending on a source declaration.
#[test]
fn a_kotlinc_compiled_provider_keeps_static_qualifier_ownership() {
    let library = "\
package provider

enum class Remote { X }
class K<T> { fun interface I { fun run() } }
";
    let Some(library) = common::kotlinc_library(library) else {
        return;
    };
    let consumer = "\
import provider.K
import provider.Remote

fun test() {
    Remote<Int>.X
    Remote<Int>.values()
    Remote<Int>.entries
    K<Int>.I { }
}
";
    let locations = [
        "Provider.kt:5:11",
        "Provider.kt:6:11",
        "Provider.kt:7:11",
        "Provider.kt:8:6",
    ];
    assert_matches_kotlinc(
        &[("Provider.kt", consumer)],
        &[library, common::stdlib_jar()],
        &[],
        &[],
        &locations.map(warning),
    );
}
