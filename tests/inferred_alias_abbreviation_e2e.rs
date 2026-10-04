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
