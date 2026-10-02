//! Unsigned varargs on the JVM.
//!
//! `vararg us: UInt` is the carrier `int[]`. A spread of `UIntArray` passes that array to the
//! spread builder; boxing it first makes `addSpread` cast a `kotlin.UIntArray` to `int[]`.
//! `vararg us: UInt?` is `kotlin.UInt[]`: each non-null element is boxed once, and `null` is stored
//! as `null`. A second `box-impl` rejects the reference the first one produced.

use super::common;

const SRC: &str = "\
fun nullable(vararg us: UInt?): Int = us.size\n\
fun callNullable(): Int = nullable(1u, null, 2u, null)\n\
fun uint(vararg us: UInt): UInt = us.sum()\n\
fun callSpread(xs: UIntArray): UInt = uint(*xs)\n\
fun callMixed(xs: UIntArray): UInt = uint(4u, *xs, 5u)\n\
fun box(): String {\n\
\x20   if (callNullable() != 4) return \"Fail nullable\"\n\
\x20   val xs = uintArrayOf(1u, 2u, 3u)\n\
\x20   if (callSpread(xs) != 6u) return \"Fail spread\"\n\
\x20   if (callMixed(xs) != 15u) return \"Fail mixed\"\n\
\x20   if (xs !is UIntArray) return \"Fail is\"\n\
\x20   return \"OK\"\n\
}\n";

#[test]
fn unsigned_varargs_match_kotlinc_at_runtime() {
    common::expect_box_same_as_kotlinc(SRC, "unsigned vararg");
}

#[test]
fn nullable_unsigned_vararg_boxes_each_present_element_once() {
    let pair = common::ModuleClassPair::compile_with_classpath(
        &[("Main.kt", SRC)],
        &[common::stdlib_jar()],
        "MainKt",
    );
    let (reference, krusty) = pair.method_code("MainKt", "callNullable");
    assert_eq!(
        krusty, reference,
        "nullable UInt vararg must box 1u and 2u once and store null directly"
    );
}

#[test]
fn an_unsigned_array_spread_keeps_the_carrier() {
    let pair = common::ModuleClassPair::compile_with_classpath(
        &[("Main.kt", SRC)],
        &[common::stdlib_jar()],
        "MainKt",
    );
    for method in ["callSpread--ajY-9A", "callMixed--ajY-9A"] {
        let (_reference, krusty) = pair.method_code("MainKt", method);
        assert!(
            !krusty.contains("UIntArray.\"box-impl\""),
            "{method} boxed the UIntArray spread:\n{krusty}"
        );
        assert!(
            krusty.contains("addSpread") || krusty.contains("copyOf"),
            "{method} did not pass the carrier array onward:\n{krusty}"
        );
    }
}
