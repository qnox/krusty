//! kotlinc's deprecation reports on import directives, and the three positions whose deprecation
//! kotlinc is migrating from a warning to the declared level: a class enclosing an imported
//! declaration's container (`ReportDeprecationsOfOuterImportedClasses`, KTLC-397), an object
//! called through the `invoke` convention (`ReportDeprecationsOfClassifiersInImplicitInvokes`,
//! KT-82456) and a companion reached through its class name as an augmented assignment's target
//! (`ReportDeprecatedCompanionInDelegation`, KTLC-408). While a migration is incomplete an `ERROR`
//! deprecation there warns, with kotlinc's "This will become an error" suffix, and no `@Suppress`
//! silences that warning.

use super::common;
use std::path::PathBuf;

const DEPENDENCY: &str = r#"package lib

@Deprecated("dep a", level = DeprecationLevel.ERROR)
class A {
    @Deprecated("dep b")
    class B {
        class X
        companion object {
            fun f() = 1
        }
    }
    class C {
        class Y
    }
}

@Deprecated("dep o", level = DeprecationLevel.ERROR)
object O {
    fun of() = 4
}

class P {
    @Deprecated("dep companion", level = DeprecationLevel.ERROR)
    companion object {
        operator fun plusAssign(x: Int) {}
    }
}
"#;

const OUTER_IMPORTED: &str = "-XXLanguage:+ReportDeprecationsOfOuterImportedClasses";
const IMPLICIT_INVOKE: &str = "-XXLanguage:+ReportDeprecationsOfClassifiersInImplicitInvokes";
const COMPANION: &str = "-XXLanguage:+ReportDeprecatedCompanionInDelegation";

const KTLC_397: &str =
    "This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-397.";

fn dependency() -> PathBuf {
    common::kotlinc_library(DEPENDENCY).expect("reference kotlinc builds the dependency")
}

/// Compile `source` against the dependency with both compilers, passing `args` to each, and
/// require the same complete errors and warnings, which must also be `errors` and `warnings`
/// (`line:column: message`, in emission order).
fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[&str], warnings: &[&str]) {
    let entries = [common::stdlib_jar(), dependency()];
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let result =
        common::compiler_diagnostics_with_shared_args(&[("Use.kt", source)], &entries, &args);
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
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
    let krusty_errors = render(&common::compiler_errors(&krusty_output));
    let kotlinc_errors = render(&common::compiler_errors(&result.reference_stderr));
    let krusty_warnings = render(&common::compiler_warnings(&krusty_output));
    let kotlinc_warnings = render(&common::compiler_warnings(&result.reference_stderr));
    assert_eq!(kotlinc_errors, errors, "kotlinc errors");
    assert_eq!(kotlinc_warnings, warnings, "kotlinc warnings");
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    assert_eq!(
        krusty_warnings, kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

const IMPORTS: &str = r#"import lib.A.B.X
import lib.A.B.Companion.f
import lib.A.C.Y
import lib.A.B.*
import lib.A.C.*
import lib.A.C.Y as Z

fun use() = f()
"#;

/// An import reports the classes its path goes through, innermost first, never the imported
/// declaration: the container at its own level, an enclosing `ERROR` class only as a warning.
#[test]
fn an_import_reports_the_classes_it_goes_through() {
    let a = format!("'class A : Any' is deprecated. dep a. {KTLC_397}");
    let b = "'class B : Any' is deprecated. dep b.";
    assert_matches_kotlinc(
        IMPORTS,
        &[],
        &[],
        &[
            &format!("1:8: {b}"),
            &format!("1:8: {a}"),
            &format!("2:8: {b}"),
            &format!("2:8: {a}"),
            &format!("3:8: {a}"),
            &format!("4:8: {b}"),
            &format!("4:8: {a}"),
            &format!("5:8: {a}"),
            &format!("6:8: {a}"),
        ],
    );
}

/// With the migration complete, an enclosing class reports at its declared level.
#[test]
fn an_outer_imported_class_reports_its_level_once_migrated() {
    let a = "'class A : Any' is deprecated. dep a.";
    assert_matches_kotlinc(
        IMPORTS,
        &[OUTER_IMPORTED],
        &[
            &format!("1:8: {a}"),
            &format!("2:8: {a}"),
            &format!("3:8: {a}"),
            &format!("4:8: {a}"),
            &format!("5:8: {a}"),
            &format!("6:8: {a}"),
        ],
        &[],
    );
}

/// The container of an imported classifier or object member reports at its level already.
#[test]
fn an_imported_declarations_container_reports_its_level() {
    assert_matches_kotlinc(
        "import lib.A.C\nimport lib.O.of\n\nfun use() = of()\n",
        &[],
        &[
            "1:8: 'class A : Any' is deprecated. dep a.",
            "2:8: 'object O : Any' is deprecated. dep o.",
        ],
        &[],
    );
}

/// `@Suppress("DEPRECATION")` silences a `WARNING` deprecation on an import but not a migration
/// warning.
#[test]
fn a_migration_warning_on_an_import_is_not_suppressed() {
    let a = format!("'class A : Any' is deprecated. dep a. {KTLC_397}");
    assert_matches_kotlinc(
        "@file:Suppress(\"DEPRECATION\")\n\nimport lib.A.B.X\nimport lib.A.C.Y\n",
        &[],
        &[],
        &[&format!("3:8: {a}"), &format!("4:8: {a}")],
    );
}

const INVOKE: &str = r#"@Deprecated("dep d", level = DeprecationLevel.ERROR)
object D {
    operator fun invoke() = 2
}

object S {
    operator fun invoke() = 1
}

fun use() = S() + D()

@Suppress("DEPRECATION")
fun suppressed() = D()
"#;

/// A deprecated object called through `invoke` reports at its name; an `ERROR` one only warns, even
/// where `DEPRECATION` is suppressed, until the migration completes.
#[test]
fn an_object_called_through_invoke_reports_its_deprecation() {
    let d = "'object D : Any' is deprecated. dep d. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KT-82456.";
    assert_matches_kotlinc(
        INVOKE,
        &[],
        &[],
        &[&format!("10:19: {d}"), &format!("13:20: {d}")],
    );
    assert_matches_kotlinc(
        INVOKE,
        &[IMPLICIT_INVOKE],
        &[
            "10:19: 'object D : Any' is deprecated. dep d.",
            "13:20: 'object D : Any' is deprecated. dep d.",
        ],
        &[],
    );
}

const AUGMENTED: &str = "import lib.P\n\nfun use() {\n    P += 1\n}\n";

/// A deprecated companion reached through its class name as an augmented assignment's target
/// reports at the written name.
#[test]
fn a_companion_augmented_through_its_class_reports_its_deprecation() {
    assert_matches_kotlinc(
        AUGMENTED,
        &[],
        &[],
        &["4:5: 'companion object Companion : Any' is deprecated. dep companion. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-408."],
    );
    assert_matches_kotlinc(
        AUGMENTED,
        &[COMPANION],
        &["4:5: 'companion object Companion : Any' is deprecated. dep companion."],
        &[],
    );
}
