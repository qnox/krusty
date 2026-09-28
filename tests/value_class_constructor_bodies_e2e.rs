//! A value class's constructors run as static `constructor-impl` functions over the carrier.
//! kotlinc first stores the constructed value in an unnamed temporary: the `init` block reads
//! `this` and its property through it, and a `this` passed as a reference is boxed with
//! `box-impl`. A class declared in a secondary constructor is enclosed by that constructor's
//! `constructor-impl`. krusty boxed `this` as the carrier's wrapper and had no enclosing method
//! for such a class.

use super::common;

const SRC: &str = "var sink: Any? = null\n\
    var last = 0\n\
    var label = \"\"\n\
    @JvmInline value class Size(val x: Int) {\n\
    \x20   constructor(s: String) : this(s.length) {\n\
    \x20       sink = this\n\
    \x20       class Local(val text: String) { init { label = text } }\n\
    \x20       Local(s)\n\
    \x20   }\n\
    \x20   init {\n\
    \x20       sink = this\n\
    \x20       last = x\n\
    \x20   }\n\
    }\n\
    fun box(): String {\n\
    \x20   Size(3)\n\
    \x20   if (sink != Size(3) || last != 3) return \"fail primary\"\n\
    \x20   Size(\"abcd\")\n\
    \x20   if (sink != Size(4) || last != 4 || label != \"abcd\") return \"fail secondary\"\n\
    \x20   return \"OK\"\n\
    }\n";

fn built(class: &str) -> common::ReferenceComparison {
    common::compare_with_kotlinc_plugin(
        "ConstructorBodies",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned")
}

#[test]
fn a_value_class_init_block_reads_this_through_a_temporary() {
    let built = built("Size");
    let method = "public static int constructor-impl(int)";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_class_in_a_value_class_constructor_is_enclosed_by_its_constructor_impl() {
    let built = built("Size$Local");
    // The constant-pool indices differ; the referenced method is javap's trailing comment.
    let enclosing = |disassembly: &str| {
        disassembly
            .lines()
            .filter(|line| line.starts_with("EnclosingMethod:"))
            .map(|line| {
                line.split_once("//")
                    .map(|(_, target)| target.trim().to_owned())
            })
            .collect::<Vec<_>>()
    };
    let reference = enclosing(&built.reference);
    assert_eq!(reference, [Some("Size.constructor-impl".to_owned())]);
    assert_eq!(enclosing(&built.krusty), reference);
}

#[test]
fn value_class_constructor_bodies_run() {
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}
