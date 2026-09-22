//! A data class's `toString` RECIPE is seeded under its simple name.
//!
//! Two paths build the `makeConcatWithConstants` recipe: the lowering, and the pool-seeding path.
//! Both now take the name from the classifier's own identity — `TypeName::nested_segment_ref` —
//! so they agree on one spelling and intern one constant.
//!
//! They did not. The lowering was already right, so the rendered string was right (`Inner(id=1)`)
//! and `nested_data_class_tostring_e2e` pins that. The seeding path split on `/` alone, so a
//! nested class kept its outer prefix: a recipe that is never invoked, but that is interned and
//! takes a `BootstrapMethods` entry of its own, leaving the class with a dead nested-name constant
//! and two bootstrap methods where kotlinc has one. This pins the bytes rather than the value.
use super::common;

const SRC: &str = "class Holder {\n\
                   \x20   data class Inner(val id: Int)\n\
                   }\n\
                   \n\
                   data class Top(val name: String)\n";

/// Both compiled for one target, so the JVM-9+ `makeConcatWithConstants` fork is the one compared.
fn built(class: &str) -> Option<Result<(), String>> {
    common::byte_diff_against_kotlinc_cp_target(
        "NestedDataToString",
        SRC,
        class,
        &[common::stdlib_jar()],
        Some("25"),
    )
}

#[test]
fn a_nested_data_class_renders_its_simple_name() {
    built("Holder$Inner")
        .expect("the reference compiler must be available to this regression")
        .expect("a nested data class must be byte-identical to kotlinc's");
}

/// The control: a top-level data class was already right, so this pins that the fix did not
/// change it — the rule is "drop the OUTER prefix", not "shorten the name".
#[test]
fn a_top_level_data_class_is_unchanged() {
    built("Top")
        .expect("the reference compiler must be available to this regression")
        .expect("a top-level data class must stay byte-identical to kotlinc's");
}
