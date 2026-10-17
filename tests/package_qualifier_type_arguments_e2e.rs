//! kotlinc's `TypeArgumentsInPackagesTypeRefChecker` for type arguments written on a package
//! segment of a qualified type or annotation reference: a deprecation warning on the segment's
//! `<…>` list until `ForbidUselessTypeArgumentsIn25` makes it an error, and always an error on the
//! last package segment.

use super::common;

const FORBID: &str = "-XXLanguage:+ForbidUselessTypeArgumentsIn25";
const MESSAGE: &str = "type arguments are not allowed for packages.";
const DEPRECATION: &str =
    "This will become an error in language version 2.5. See https://youtrack.jetbrains.com/issue/KTLC-390.";

fn warning(location: &str) -> String {
    format!("{location}: {MESSAGE} {DEPRECATION}")
}

fn error(location: &str) -> String {
    format!("{location}: {MESSAGE}")
}

/// Compile `sources` with both compilers, passing `args` to each, and require the same complete
/// errors and warnings, which must also be `errors` and `warnings`.
fn assert_matches_kotlinc(
    sources: &[(&str, &str)],
    args: &[&str],
    errors: &[String],
    warnings: &[String],
) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result =
        common::compiler_diagnostics_with_shared_args(sources, &[common::stdlib_jar()], &args);
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

const PACKAGES: &str = "\
package part1.part2.part3

annotation class Anno
class Cls
class Gen<T>
class Outer<T> {
    inner class Inner
}

@part1<Int>.part2.part3.Anno
fun target1() {}

@part1.part2<Int>.part3.Anno
fun target2() {}

fun types(a: part1<Int>.part2.part3.Cls, b: part1.part2<String>.part3.Gen<Int>): part1<Int>.part2.part3.Cls = a

val v: part1<Int>.part2.part3.Cls? = null
val w: List<part1<*>.part2.part3.Cls> = emptyList()
val x: part1<Int>.part2.part3.Outer<String>.Inner? = null

@Suppress(\"TYPE_ARGUMENTS_NOT_ALLOWED_WARNING\")
val s: part1<Int>.part2.part3.Cls? = null
";

const PACKAGE_LOCATIONS: [&str; 8] = [
    "Packages.kt:10:7",
    "Packages.kt:13:13",
    "Packages.kt:16:19",
    "Packages.kt:16:56",
    "Packages.kt:16:87",
    "Packages.kt:18:13",
    "Packages.kt:19:18",
    "Packages.kt:20:13",
];

#[test]
fn package_type_arguments_are_deprecated() {
    assert_matches_kotlinc(
        &[("Packages.kt", PACKAGES)],
        &[],
        &[],
        &PACKAGE_LOCATIONS.map(warning),
    );
}

#[test]
fn forbidden_package_type_arguments_are_errors() {
    let mut errors = PACKAGE_LOCATIONS.map(error).to_vec();
    errors.push(error("Packages.kt:23:13"));
    assert_matches_kotlinc(&[("Packages.kt", PACKAGES)], &[FORBID], &errors, &[]);
}

const LAST_PACKAGE: &str = "\
package part1.part2.part3

annotation class Anno
class Cls

@part1<Int>.part2<Int>.part3<Int>.Anno
fun target() {}

val v: part1.part2.part3<Int>.Cls? = null
val w: part1<Int>.part2.part3<String>.Cls? = null
";

/// The last package segment never accepted type arguments. kotlinc prints no warnings beside an
/// error, so the earlier segments' deprecations stay silent here.
#[test]
fn last_package_segment_type_arguments_are_an_error() {
    assert_matches_kotlinc(
        &[("LastPackage.kt", LAST_PACKAGE)],
        &[],
        &[
            "LastPackage.kt:6:29",
            "LastPackage.kt:9:25",
            "LastPackage.kt:10:30",
        ]
        .map(error),
        &[],
    );
}

#[test]
fn forbidden_last_package_segment_type_arguments_are_errors() {
    assert_matches_kotlinc(
        &[("LastPackage.kt", LAST_PACKAGE)],
        &[FORBID],
        &[
            "LastPackage.kt:6:7",
            "LastPackage.kt:6:18",
            "LastPackage.kt:6:29",
            "LastPackage.kt:9:25",
            "LastPackage.kt:10:13",
            "LastPackage.kt:10:30",
        ]
        .map(error),
        &[],
    );
}

/// `Flat` is one selected classifier segment even though its expansion has two. The package
/// boundary belongs to the alias binding, not to `Outer.Nested`'s expanded identity.
#[test]
fn an_alias_expansion_does_not_move_the_written_package_boundary() {
    assert_matches_kotlinc(
        &[(
            "AliasBoundary.kt",
            "\
package aliasboundary.parts

class Outer { class Nested }
typealias Flat = Outer.Nested

fun take(value: aliasboundary<Any>.parts.Flat?) {}
",
        )],
        &[],
        &[],
        &[warning("AliasBoundary.kt:6:30")],
    );
}

#[test]
fn the_last_package_segment_is_kept_for_aliases_and_escaped_names() {
    assert_matches_kotlinc(
        &[
            (
                "AliasBoundary.kt",
                "\
package aliasboundary.parts

class Outer { class Nested }
typealias Flat = Outer.Nested

fun take(value: aliasboundary.parts<Any>.Flat?) {}
",
            ),
            (
                "Escaped.kt",
                "\
package escaped.`when`

class Cls

fun take(value: escaped.`when`<Any>.Cls?) {}
",
            ),
        ],
        &[],
        &[error("AliasBoundary.kt:6:36"), error("Escaped.kt:5:31")],
        &[],
    );
}

#[test]
fn type_alias_targets_use_the_same_package_boundary_check() {
    assert_matches_kotlinc(
        &[(
            "TypeAliasTarget.kt",
            "\
package part1.part2.part3

class Cls
typealias Alias = part1<Int>.part2.part3.Cls
",
        )],
        &[],
        &[],
        &[warning("TypeAliasTarget.kt:4:24")],
    );
}

#[test]
fn contextual_local_constructor_parameters_keep_their_own_header_sites() {
    assert_matches_kotlinc(
        &[(
            "ContextualConstructor.kt",
            "\
package contextual.parts

class Marker
class Value
fun build() {
    context(marker: contextual.parts.Marker)
    class Local(value: contextual<Any>.parts.Value)
}
",
        )],
        &["-XXLanguage:+ContextParameters"],
        &[],
        &[warning("ContextualConstructor.kt:7:34")],
    );
}

#[test]
fn shape_changing_local_alias_projects_nested_source_facts_once() {
    assert_matches_kotlinc(
        &[(
            "LocalAlias.kt",
            "\
package localalias.parts

class Leaf
fun build() {
    typealias Pair<T> = Map<T, T>
    class Local(val value: Pair<localalias<Any>.parts.Leaf>)
}
",
        )],
        &["-XXLanguage:+LocalTypeAliases"],
        &[],
        &[warning("LocalAlias.kt:6:43")],
    );
}

#[test]
fn alias_expansion_keeps_its_declaration_site_target_under_use_site_shadowing() {
    assert_matches_kotlinc(
        &[(
            "AliasShadowing.kt",
            "\
package aliasshadow.parts

class Target<T>(val declarationSite: Int)
typealias Alias<T> = Target<T>
class Leaf
fun build() {
    class Target<T>
    class Local(val value: Alias<aliasshadow<Any>.parts.Leaf>) {
        fun read(): Int = value.declarationSite
    }
}
",
        )],
        &[],
        &[],
        &[warning("AliasShadowing.kt:8:45")],
    );
}

#[test]
fn local_alias_target_is_bound_before_a_later_use_site_shadow() {
    assert_matches_kotlinc(
        &[(
            "LocalAliasShadowing.kt",
            "\
package localshadow.parts

class Target<T>(val declarationSite: Int)
class Leaf
fun build() {
    typealias Alias<T> = Target<T>
    class Target<T>
    class Local(val value: Alias<localshadow<Any>.parts.Leaf>) {
        fun read(): Int = value.declarationSite
    }
}
",
        )],
        &["-XXLanguage:+LocalTypeAliases"],
        &[],
        &[warning("LocalAliasShadowing.kt:8:45")],
    );
}

#[test]
fn chained_local_aliases_reuse_the_first_alias_semantic_template() {
    assert_matches_kotlinc(
        &[(
            "LocalAliasChain.kt",
            "\
package localchain.parts

class Target<T>(val declarationSite: Int)
class Leaf
fun build() {
    typealias A<T> = Target<T>
    typealias B<T> = A<T>
    class Target<T>
    class Local(val value: B<localchain<Any>.parts.Leaf>) {
        fun read(): Int = value.declarationSite
    }
}
",
        )],
        &["-XXLanguage:+LocalTypeAliases"],
        &[],
        &[warning("LocalAliasChain.kt:9:40")],
    );
}

#[test]
fn non_generic_local_alias_chain_b_equals_a_keeps_a_bound_target() {
    assert_matches_kotlinc(
        &[(
            "LocalAliasSimpleChain.kt",
            "\
class Target(val declarationSite: Int)
fun build() {
    typealias A = Target
    typealias B = A
    class Target
    class Local(val value: B) {
        fun read(): Int = value.declarationSite
    }
}
",
        )],
        &["-XXLanguage:+LocalTypeAliases"],
        &[],
        &[],
    );
}

#[test]
fn inferred_result_evaluates_a_generic_cast_type_structurally() {
    assert_matches_kotlinc(
        &[(
            "InferredGenericCast.kt",
            "\
class Cell<T>(val value: T)

fun inferred(input: Cell<String>?) = (input as Cell<String>).value
fun use(): Int = inferred(Cell(\"OK\")).length
",
        )],
        &[],
        &[],
        &[],
    );
}

#[test]
fn inferred_result_uses_a_bound_local_alias_inside_the_body() {
    assert_matches_kotlinc(
        &[(
            "InferredLocalAlias.kt",
            "\
class Cell<T>(val value: T)

fun inferred(input: Cell<String>?) = run {
    typealias Alias<T> = Cell<T>
    (input as Alias<String>).value
}

fun use(): Int = inferred(Cell(\"OK\")).length
",
        )],
        &["-XXLanguage:+LocalTypeAliases"],
        &[],
        &[],
    );
}

const INVALID_PACKAGE_ARGUMENTS: &str = "\
package retained.parts

class C<T>
annotation class A

val ordinary: retained<Missing>.parts.C<OtherMissing>? = null

@retained<AnnotationMissing>.parts.A
val annotated = 1
";

/// Package-owned argument values do not enter classifier application, but their source type
/// occurrences still belong to ordinary resolution. Annotation names retain the same syntax even
/// though they have no classifier `TypeRef::targs` of their own.
#[test]
fn package_argument_types_keep_their_exact_diagnostics() {
    let unresolved = [
        "InvalidPackageArguments.kt:6:24: unresolved reference 'Missing'.".to_string(),
        "InvalidPackageArguments.kt:6:41: unresolved reference 'OtherMissing'.".to_string(),
        "InvalidPackageArguments.kt:8:11: unresolved reference 'AnnotationMissing'.".to_string(),
    ];
    assert_matches_kotlinc(
        &[("InvalidPackageArguments.kt", INVALID_PACKAGE_ARGUMENTS)],
        &[],
        &unresolved,
        &[],
    );

    assert_matches_kotlinc(
        &[("InvalidPackageArguments.kt", INVALID_PACKAGE_ARGUMENTS)],
        &[FORBID],
        &[
            error("InvalidPackageArguments.kt:6:23"),
            unresolved[0].clone(),
            unresolved[1].clone(),
            error("InvalidPackageArguments.kt:8:10"),
            unresolved[2].clone(),
        ],
        &[],
    );
}
