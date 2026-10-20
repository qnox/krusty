//! Import-path deprecations and declaration-use positions migrating to errors in Kotlin 2.5.

use super::common;
use krusty::kotlin_version::KotlinVersion;
use std::path::PathBuf;

const DEPENDENCY: &str = r#"package lib

import kotlin.reflect.KProperty

@Deprecated("dep a", level = DeprecationLevel.ERROR)
class A {
    @Deprecated("dep b")
    class B {
        class X
        companion object { fun f() = 1 }
    }
    class C { class Y }
}

class P {
    @Deprecated("dep companion", level = DeprecationLevel.ERROR)
    companion object {
        operator fun getValue(thisRef: Any?, property: KProperty<*>) = ""
        operator fun plusAssign(x: Int) {}
        fun baz(x: String): Int = 1
    }
}

@Deprecated("warning a")
class WarningA {
    class C { class Y }
}

class WarningP {
    @Deprecated("warning companion")
    companion object { operator fun plusAssign(x: Int) {} }
}
"#;

const OUTER_IMPORTED: &str = "-XXLanguage:+ReportDeprecationsOfOuterImportedClasses";
const IMPLICIT_INVOKE: &str = "-XXLanguage:+ReportDeprecationsOfClassifiersInImplicitInvokes";
const COMPANION: &str = "-XXLanguage:+ReportDeprecatedCompanionInDelegation";

fn dependency() -> PathBuf {
    common::kotlinc_library(DEPENDENCY).expect("reference kotlinc builds the dependency")
}

fn assert_matches_kotlinc(
    sources: &[(&str, &str)],
    classpath: &[PathBuf],
    args: &[&str],
    errors: &[&str],
    warnings: &[&str],
) {
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
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
    let krusty_errors = render(&common::compiler_errors(&krusty_output));
    let reference_errors = render(&common::compiler_errors(&result.reference_stderr));
    let krusty_warnings = render(&common::compiler_warnings(&krusty_output));
    let reference_warnings = render(&common::compiler_warnings(&result.reference_stderr));
    assert_eq!(reference_errors, errors, "kotlinc errors");
    assert_eq!(reference_warnings, warnings, "kotlinc warnings");
    assert_eq!(krusty_errors, reference_errors, "errors\n{krusty_output}");
    assert_eq!(
        krusty_warnings, reference_warnings,
        "warnings\n{krusty_output}"
    );
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

#[test]
fn import_classifier_chain_reports_complete_order_and_count() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let source = r#"import lib.A.B.X
import lib.A.B.Companion.f
import lib.A.C.Y
import lib.A.B.*
import lib.A.C.*
import lib.A.C.Y as Z

fun use() = f()
"#;
    let a = "'class A : Any' is deprecated. dep a. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-397.";
    let b = "'class B : Any' is deprecated. dep b.";
    assert_matches_kotlinc(
        &[("Use.kt", source)],
        &[common::stdlib_jar(), dependency()],
        &[],
        &[],
        &[
            &format!("Use.kt:1:8: {b}"),
            &format!("Use.kt:1:8: {a}"),
            &format!("Use.kt:2:8: {b}"),
            &format!("Use.kt:2:8: {a}"),
            &format!("Use.kt:3:8: {a}"),
            &format!("Use.kt:4:8: {b}"),
            &format!("Use.kt:4:8: {a}"),
            &format!("Use.kt:5:8: {a}"),
            &format!("Use.kt:6:8: {a}"),
        ],
    );
}

#[test]
fn source_import_reports_published_outer_but_not_imported_classifier_deprecation() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    assert_matches_kotlinc(
        &[
            (
                "Declaration.kt",
                "package source\n@Deprecated(\"source outer\")\nclass Outer {\n    @Deprecated(\"source imported\")\n    class Imported\n}\n",
            ),
            ("Use.kt", "import source.Outer.Imported\n"),
        ],
        &[common::stdlib_jar()],
        &[],
        &[],
        &["Use.kt:1:8: 'class Outer : Any' is deprecated. source outer."],
    );
}

#[test]
fn file_suppression_uses_published_policy_but_does_not_hide_migration_warning() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    assert_matches_kotlinc(
        &[(
            "Use.kt",
            "@file:Suppress(\"DEPRECATION\", \"DEPRECATION_ERROR\")\n\nimport lib.A.B.X\n",
        )],
        &[common::stdlib_jar(), dependency()],
        &[],
        &[],
        &[
            "Use.kt:3:8: 'class A : Any' is deprecated. dep a. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-397.",
        ],
    );
}

const MIGRATING_USES: &str = r#"import lib.A.C.Y
import lib.P

@Deprecated("dep o", level = DeprecationLevel.ERROR)
object O {
    operator fun invoke() = 4
}

fun use() {
    O()
    P += 1
}
"#;

const MIGRATION_WARNINGS: &[&str] = &[
    "Use.kt:1:8: 'class A : Any' is deprecated. dep a. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-397.",
    "Use.kt:10:5: 'object O : Any' is deprecated. dep o. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KT-82456.",
    "Use.kt:11:5: 'companion object Companion : Any' is deprecated. dep companion. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-408.",
];

const WARNING_MIGRATING_USES: &str = r#"import lib.WarningA.C.Y
import lib.WarningP

@Deprecated("warning o")
object WarningO {
    operator fun invoke() = 4
}

fun use() {
    WarningO()
    WarningP += 1
}
"#;

const ORDINARY_WARNINGS_IN_MIGRATING_POSITIONS: &[&str] = &[
    "Use.kt:1:8: 'class WarningA : Any' is deprecated. warning a.",
    "Use.kt:10:5: 'object WarningO : Any' is deprecated. warning o.",
    "Use.kt:11:5: 'companion object Companion : Any' is deprecated. warning companion.",
];

#[test]
fn migrating_positions_follow_the_selected_releases_exact_diagnostics() {
    let warnings: &[&str] = if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        &[]
    } else {
        MIGRATION_WARNINGS
    };
    assert_matches_kotlinc(
        &[("Use.kt", MIGRATING_USES)],
        &[common::stdlib_jar(), dependency()],
        &[],
        &[],
        warnings,
    );
}

#[test]
fn warning_deprecations_follow_the_selected_releases_migration_checker() {
    let warnings: &[&str] = if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        &[]
    } else {
        ORDINARY_WARNINGS_IN_MIGRATING_POSITIONS
    };
    assert_matches_kotlinc(
        &[("Use.kt", WARNING_MIGRATING_USES)],
        &[common::stdlib_jar(), dependency()],
        &[],
        &[],
        warnings,
    );
}

/// Migration-period warnings retain kotlinc's diagnostic identity so their named CLI policy can
/// override global warning policy without changing the source-level suppression contract.
#[test]
fn named_warning_policy_controls_deprecation_error_migration_period() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    struct Case {
        arguments: &'static [&'static str],
        errors: &'static [&'static str],
        warnings: &'static [&'static str],
    }
    let cases = [
        Case {
            arguments: &["-Xwarning-level=DEPRECATION_ERROR_MIGRATION_PERIOD_WARNING:disabled"],
            errors: &[],
            warnings: &[],
        },
        Case {
            arguments: &["-Xwarning-level=DEPRECATION_ERROR_MIGRATION_PERIOD_WARNING:error"],
            errors: MIGRATION_WARNINGS,
            warnings: &[],
        },
        Case {
            arguments: &[
                "-Werror",
                "-Xwarning-level=DEPRECATION_ERROR_MIGRATION_PERIOD_WARNING:warning",
            ],
            errors: &[],
            warnings: MIGRATION_WARNINGS,
        },
        Case {
            arguments: &[
                "-nowarn",
                "-Xwarning-level=DEPRECATION_ERROR_MIGRATION_PERIOD_WARNING:warning",
            ],
            errors: &[],
            warnings: MIGRATION_WARNINGS,
        },
    ];
    let classpath = [common::stdlib_jar(), dependency()];
    for case in cases {
        assert_matches_kotlinc(
            &[("Use.kt", MIGRATING_USES)],
            &classpath,
            case.arguments,
            case.errors,
            case.warnings,
        );
    }
}

#[test]
fn implicit_invoke_reports_the_selected_owner_under_its_migration_policy() {
    let source = r#"@Deprecated("deprecated owner", level = DeprecationLevel.ERROR)
interface Factory {
    companion object { operator fun invoke() = 1 }
}

fun use() = Factory()
"#;
    let warnings: &[&str] = if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        &[]
    } else {
        &["Use.kt:6:13: 'interface Factory : Any' is deprecated. deprecated owner. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KT-82456."]
    };
    assert_matches_kotlinc(
        &[("Use.kt", source)],
        &[common::stdlib_jar()],
        &[],
        &[],
        warnings,
    );
    if krusty::kotlin_version::target() >= KotlinVersion::V2_4_20 {
        assert_matches_kotlinc(
            &[("Use.kt", source)],
            &[common::stdlib_jar()],
            &[IMPLICIT_INVOKE],
            &["Use.kt:6:13: 'interface Factory : Any' is deprecated. deprecated owner."],
            &[],
        );
    }
}

#[test]
fn implicit_invoke_keeps_companion_deprecation_at_its_ordinary_level() {
    let source = r#"interface Factory {
    @Deprecated("deprecated companion", level = DeprecationLevel.ERROR)
    companion object { operator fun invoke() = 1 }
}

fun use() = Factory()
"#;
    assert_matches_kotlinc(
        &[("Use.kt", source)],
        &[common::stdlib_jar()],
        &[],
        &["Use.kt:6:13: 'companion object Companion : Any' is deprecated. deprecated companion."],
        &[],
    );
}

#[test]
fn delegated_property_companion_positions_follow_the_migration_policy() {
    let source = r#"import lib.P

val topLevel by P

fun use() {
    val local by P
    P += 1
}
"#;
    let warnings: &[&str] = if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        &[]
    } else {
        &[
            "Use.kt:3:17: 'companion object Companion : Any' is deprecated. dep companion. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-408.",
            "Use.kt:6:18: 'companion object Companion : Any' is deprecated. dep companion. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-408.",
            "Use.kt:7:5: 'companion object Companion : Any' is deprecated. dep companion. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-408.",
        ]
    };
    assert_matches_kotlinc(
        &[("Use.kt", source)],
        &[common::stdlib_jar(), dependency()],
        &[],
        &[],
        warnings,
    );
    if krusty::kotlin_version::target() >= KotlinVersion::V2_4_20 {
        assert_matches_kotlinc(
            &[("Use.kt", source)],
            &[common::stdlib_jar(), dependency()],
            &[COMPANION],
            &[
                "Use.kt:3:17: 'companion object Companion : Any' is deprecated. dep companion.",
                "Use.kt:6:18: 'companion object Companion : Any' is deprecated. dep companion.",
                "Use.kt:7:5: 'companion object Companion : Any' is deprecated. dep companion.",
            ],
            &[],
        );
    }
}

#[test]
fn callable_reference_qualifier_reports_the_selected_companion_ordinary_deprecation() {
    let source = r#"import lib.P

fun use() {
    val reference: (String) -> Int = P::baz
}
"#;
    assert_matches_kotlinc(
        &[("Use.kt", source)],
        &[common::stdlib_jar(), dependency()],
        &[],
        &["Use.kt:4:38: 'companion object Companion : Any' is deprecated. dep companion."],
        &[],
    );
}

#[test]
fn completed_migrations_restore_declared_error_levels() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    assert_matches_kotlinc(
        &[("Use.kt", MIGRATING_USES)],
        &[common::stdlib_jar(), dependency()],
        &[OUTER_IMPORTED, IMPLICIT_INVOKE, COMPANION],
        &[
            "Use.kt:1:8: 'class A : Any' is deprecated. dep a.",
            "Use.kt:10:5: 'object O : Any' is deprecated. dep o.",
            "Use.kt:11:5: 'companion object Companion : Any' is deprecated. dep companion.",
        ],
        &[],
    );
}

#[test]
fn qualified_companion_assignment_keeps_the_written_target_span() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    assert_matches_kotlinc(
        &[("Use.kt", "fun use() {\n    lib.P += 1\n}\n")],
        &[common::stdlib_jar(), dependency()],
        &[],
        &[],
        &[
            "Use.kt:2:9: 'companion object Companion : Any' is deprecated. dep companion. This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-408.",
        ],
    );
}
