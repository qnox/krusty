//! A value class's sole property of another value class's type is that value class, carried as
//! its carrier. `o.toString()` on it calls the nested class's static `toString-impl` over the
//! carrier, as kotlinc does; krusty boxed the carrier as a `Long` and printed the bare number.
//! The outer class's own synthesized `toString-impl` does the same, so `Outer(Inner(20))` prints
//! `Outer(i=Inner(x=20))` rather than `Outer(i=20)`.

use super::common;

const SRC: &str = "@JvmInline value class Inner(val l: Long)\n\
    @JvmInline value class Outer(val o: Inner) {\n\
    \x20   fun show(): String = o.toString()\n\
    }\n";

#[test]
fn a_nested_value_class_property_is_printed_by_its_class() {
    let built = common::compare_with_kotlinc_plugin(
        "NestedPropertyToString",
        SRC,
        "Outer",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let method = "public static final java.lang.String show-impl(long)";
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes show-impl");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference
    );
}

#[test]
fn a_nested_value_class_property_prints_its_class() {
    let src = format!(
        "{SRC}fun box(): String = if (Outer(Inner(6)).show() == \"Inner(l=6)\") \"OK\" else \"fail\"\n"
    );
    assert_eq!(common::expect_box_run_with_stdlib(&src, "Main"), "OK");
}

const NESTED_TEXT: &str = "@JvmInline value class Inner(val x: Long)\n\
    @JvmInline value class Outer(val i: Inner)\n\
    @JvmInline value class Name(val s: String)\n\
    @JvmInline value class Wrap(val n: Name)\n\
    @JvmInline value class Maybe(val i: Inner?)\n";

#[test]
fn a_synthesized_to_string_prints_the_nested_value_class() {
    for target in ["1.8", "25"] {
        for (class, marker) in [
            ("Outer", "java.lang.String toString-impl(long)"),
            ("Wrap", "java.lang.String toString-impl(java.lang.String)"),
            ("Maybe", "java.lang.String toString-impl(Inner)"),
        ] {
            let built = common::compare_with_kotlinc_plugin(
                "NestedValueToString",
                NESTED_TEXT,
                class,
                &[common::stdlib_jar()],
                target,
                &[],
            )
            .expect("reference kotlinc is provisioned");
            let reference = common::method_instructions(&built.reference, marker);
            assert!(
                !reference.is_empty(),
                "{target} {class}: kotlinc writes {marker}"
            );
            assert_eq!(
                common::method_instructions(&built.krusty, marker),
                reference,
                "{target} {class}"
            );
        }
    }
}

#[test]
fn a_synthesized_to_string_names_the_nested_class() {
    let src = format!(
        "{NESTED_TEXT}fun box(): String {{\n\
         \x20   val outer = Outer(Inner(20)).toString()\n\
         \x20   if (outer != \"Outer(i=Inner(x=20))\") return \"FAIL outer \" + outer\n\
         \x20   val wrap = Wrap(Name(\"hi\")).toString()\n\
         \x20   if (wrap != \"Wrap(n=Name(s=hi))\") return \"FAIL wrap \" + wrap\n\
         \x20   val some = Maybe(Inner(1)).toString()\n\
         \x20   if (some != \"Maybe(i=Inner(x=1))\") return \"FAIL some \" + some\n\
         \x20   val none = Maybe(null).toString()\n\
         \x20   if (none != \"Maybe(i=null)\") return \"FAIL none \" + none\n\
         \x20   return \"OK\"\n\
         }}\n"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(&src, "NestedValueToStringRun"),
        "OK"
    );
}
