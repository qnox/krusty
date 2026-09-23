//! Two more of the array members the runtime answers, and what the table's carried types are for.
//!
//! Every operand on this path used to cross as a reference, which is right for all of them but
//! one: `copyOf`'s size is an `Int`, and boxing it to hand it over is not a slow path but a
//! signature the verifier rejects. The call site now carries each operand as the table says.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// `copyOf` answers an array of the receiver's OWN kind, padded with the element type's zero.
#[test]
fn an_array_copies_itself_to_a_length() {
    let source = r#"
fun box(): String {
    val a = intArrayOf(1, 2, 3)
    if (a.copyOf().size != 3 || a.copyOf()[2] != 3) return "fail same"
    val shorter = a.copyOf(2)
    if (shorter.size != 2 || shorter[1] != 2) return "fail shorter"
    val longer = a.copyOf(5)
    if (longer.size != 5 || longer[2] != 3 || longer[3] != 0) return "fail longer"
    a[0] = 9
    if (shorter[0] != 1) return "fail copy is not a view"

    val refs = arrayOf("a", "b")
    val padded = refs.copyOf(3)
    if (padded.size != 3 || padded[0] != "a" || padded[2] != null) return "fail references"

    val flags = booleanArrayOf(true).copyOf(2)
    if (flags.size != 2 || !flags[0] || flags[1]) return "fail booleans"
    if (intArrayOf().copyOf(2).size != 2) return "fail from empty"
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_array_copy_of", source);
}

/// `contentDeepToString` recurses into nested arrays, and renders a cycle as `[...]`.
#[test]
fn an_array_renders_its_contents_deeply() {
    let source = r#"
fun box(): String {
    val nested = arrayOf(arrayOf(1, 2), arrayOf(3))
    if (nested.contentDeepToString() != "[[1, 2], [3]]") {
        return "fail nested " + nested.contentDeepToString()
    }
    val mixed = arrayOf<Any?>(1, "a", null, intArrayOf(7, 8))
    if (mixed.contentDeepToString() != "[1, a, null, [7, 8]]") {
        return "fail mixed " + mixed.contentDeepToString()
    }
    if (arrayOf<Any?>().contentDeepToString() != "[]") return "fail empty"

    val cyclic = arrayOfNulls<Any?>(2)
    cyclic[0] = 1
    cyclic[1] = cyclic
    if (cyclic.contentDeepToString() != "[1, [...]]") {
        return "fail cycle " + cyclic.contentDeepToString()
    }
    return "OK"
}
"#;
    every_backend_agrees_with_kotlinc("native_array_deep_to_string", source);
}
