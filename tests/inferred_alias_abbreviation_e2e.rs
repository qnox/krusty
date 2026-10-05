//! An inferred declaration type reached through a typealias keeps the alias as its abbreviation.
//!
//! kotlinc gives a constructor call through a typealias the alias as an abbreviation attribute on
//! its type, and an implicitly typed declaration inherits that type, so its `@Metadata` return
//! type records `abbreviatedType` exactly as if the alias had been written.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    let result = common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

const PRELUDE: &str = "package app\n\
    \n\
    class Payload\n\
    class Crate<T>(val content: T)\n\
    typealias Cargo = Payload\n\
    typealias Boxed<T> = Crate<T>\n\
    typealias Stacked<T> = Crate<Crate<T>>\n\
    val flag = true\n";

#[test]
fn an_inferred_property_and_function_keep_the_alias() {
    let src = format!(
        "{PRELUDE}\n\
         val stored = Cargo()\n\
         fun made() = Cargo()\n"
    );
    assert_identical("AliasTop", &src, "app/AliasTopKt");
}

#[test]
fn an_inferred_member_property_keeps_the_alias() {
    let src = format!("{PRELUDE}\nclass Host {{ val stored = Cargo() }}\n");
    assert_identical("AliasMember", &src, "app/Host");
}

#[test]
fn a_generic_alias_keeps_its_inferred_and_explicit_arguments() {
    let src = format!(
        "{PRELUDE}\n\
         val inferred = Boxed(Payload())\n\
         val explicit = Boxed<Payload>(Payload())\n\
         val nested = Stacked(Crate(Payload()))\n"
    );
    assert_identical("AliasGeneric", &src, "app/AliasGenericKt");
}

/// A join keeps the abbreviation of the first branch whose type is the joined type: two aliased
/// branches keep it, a leading unaliased branch does not, and a joined nullable type is new.
#[test]
fn a_join_keeps_the_abbreviation_only_of_an_equal_first_branch() {
    let src = format!(
        "{PRELUDE}\n\
         val both = if (flag) Cargo() else Cargo()\n\
         val mixed = if (flag) Payload() else Cargo()\n\
         val nullable = if (flag) Cargo() else null\n"
    );
    assert_identical("AliasJoin", &src, "app/AliasJoinKt");
}

const DEPENDENCY: &str = "package dep\n\
    \n\
    class Payload\n\
    class Crate<T>(val content: T)\n\
    typealias Cargo = Payload\n\
    typealias Boxed<T> = Crate<T>\n";

fn assert_identical_over_dependency(stem: &str, src: &str, class_internal: &str) {
    let result = common::metadata_diff_against_kotlinc_lib(
        stem,
        &[("Dep.kt", DEPENDENCY)],
        src,
        class_internal,
    )
    .expect("reference kotlinc is provisioned");
    result.unwrap_or_else(|diff| panic!("{diff}"));
}

/// A constructor call through an explicitly imported alias keeps that alias.
#[test]
fn an_imported_alias_constructor_keeps_the_alias() {
    let src = "package app\n\
        \n\
        import dep.Cargo\n\
        \n\
        val stored = Cargo()\n";
    assert_identical_over_dependency("ImportedAlias", src, "app/ImportedAliasKt");
}

/// A package-qualified constructor call binds the alias declaration itself, so qualification keeps
/// the alias; a generic alias keeps its explicit argument the same way.
#[test]
fn a_package_qualified_alias_constructor_keeps_the_alias() {
    let src = "package app\n\
        \n\
        val stored = dep.Cargo()\n\
        val boxed = dep.Boxed<dep.Payload>(dep.Payload())\n";
    assert_identical_over_dependency("QualifiedAlias", src, "app/QualifiedAliasKt");
}

/// A nearer classifier root named like the package commits the path to that classifier: the
/// nested class it reaches names no alias.
#[test]
fn a_classifier_root_shadowing_the_alias_package_names_no_alias() {
    let src = "package app\n\
        \n\
        object dep { class Cargo }\n\
        val stored = dep.Cargo()\n";
    assert_identical_over_dependency("ClassifierRoot", src, "app/ClassifierRootKt");
}

/// A nearer value root named like the package commits the path to that value: the call is its
/// member, not the alias constructor.
#[test]
fn a_value_root_shadowing_the_alias_package_names_no_alias() {
    let src = "package app\n\
        \n\
        class Maker { fun Cargo() = 1 }\n\
        val dep = Maker()\n\
        val stored = dep.Cargo()\n";
    assert_identical_over_dependency("ValueRoot", src, "app/ValueRootKt");
}

/// A nearer nested classifier owns the spelling: its constructor's result names no alias even
/// though a same-named alias is declared in the package.
#[test]
fn a_nested_classifier_shadowing_a_package_alias_names_no_alias() {
    let src = format!("{PRELUDE}\nclass Host {{\n    class Cargo\n    val stored = Cargo()\n}}\n");
    assert_identical("AliasShadowed", &src, "app/Host");
}

/// The same for an explicitly imported alias.
#[test]
fn a_nested_classifier_shadowing_an_imported_alias_names_no_alias() {
    let src = "package app\n\
        \n\
        import dep.Cargo\n\
        \n\
        class Host {\n\
            class Cargo\n\
            val stored = Cargo()\n\
        }\n";
    assert_identical_over_dependency("ImportShadowed", src, "app/Host");
}

/// An inferred type reached through an annotated typealias carries the right-hand side's type-use
/// annotations on its expanded type, as an explicitly written use of the alias does.
#[test]
fn an_inferred_annotated_alias_keeps_its_annotations() {
    let src = "package app\n\
        \n\
        @Target(AnnotationTarget.TYPE)\n\
        annotation class Kept\n\
        class Payload\n\
        typealias Marked = @Kept Payload\n\
        val stored = Marked()\n\
        fun made() = Marked()\n";
    assert_identical("AliasAnnotated", src, "app/AliasAnnotatedKt");
}
