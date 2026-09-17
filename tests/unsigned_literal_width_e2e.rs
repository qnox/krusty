//! An unsigned integer literal takes its WIDTH from the type its context requires.
//!
//! Kotlin types an integer literal from its context whether or not it carries a `u`: `3u` in a
//! `ULong` position is a `ULong` literal, the same way `3` in a `Long` position is a `Long` one.
//! krusty's signed literals already did this; the unsigned ones emitted a `UInt` constant
//! unconditionally, so a `3u` reaching a 64-bit position carried 32 bits.
//!
//! The JVM backend then emitted a class file that does not LOAD. `f(3u)` for `fun f(v: ULong)`
//! pushed `iconst_3` into a `(J)V` call, and the verifier rejected the method:
//!
//! ```text
//! VerifyError: Bad type on operand stack
//!   Reason: Type integer (current frame, stack[0]) is not assignable to long_2nd
//!   Bytecode: 06 b8 0018 b0        // iconst_3; invokestatic f:(J)Ljava/lang/String;; areturn
//! ```
//!
//! An ASSIGNMENT was already correct — `val x: ULong = 3u` converts at the initializer — which is
//! what kept this hidden: only a literal reaching a PARAMETER, an argument or a collection element
//! carried the wrong width, and those are exactly the positions no test covered.
//!
//! Every expectation here is kotlinc's, taken by compiling and running the same `box()` under it.

use super::common;

fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "C")
}

/// The one-line program the defect reduces to, and the shapes around it.
///
/// `assign` passed before this fix and is kept because it is the case that hid the others: a
/// reader has to see that the initializer position was never the broken one.
#[test]
fn an_unsigned_literal_reaching_a_wider_parameter_carries_that_width() {
    const SRC: &str =
        "fun f(v: ULong): String = if (v == 3uL) \"OK\" else \"bad \" + v.toString()\n\
fun box(): String {\n\
    val assign: ULong = 3u\n\
    if (assign != 3uL) return \"fail assign\"\n\
    if (f(3u) != \"OK\") return \"fail argument\"\n\
    if (f(3uL) != \"OK\") return \"fail explicit\"\n\
    val elements = listOf<ULong>(3u, 4u, 5u)\n\
    if (elements[0] != 3uL) return \"fail element\"\n\
    if (elements != listOf<ULong>(3uL, 4uL, 5uL)) return \"fail list\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

/// The widths that must NOT change, so the fix cannot be a blanket widening.
///
/// `UByte` and `UShort` are carried by a `UInt` constant because [`FirConstant`] has no narrower
/// unsigned variant and neither needs one — both are at most 32 bits. A literal left at `UInt`
/// where `UInt` is wanted is the ordinary case and by far the most common, so it is pinned too,
/// including at `UInt`'s maximum where the top bit is set.
///
/// The value is read back through `toString` rather than through `==`, and that is deliberate: two
/// equal `UByte` values compare UNEQUAL on this backend today (`200u` passed to a `UByte`
/// parameter is not `200.toUByte()`, though both render `200`), which is a separate defect in the
/// narrow unsigned comparison and not something this change touches. Asserting equality here would
/// fail for that unrelated reason and hide what this test is for.
#[test]
fn the_narrower_unsigned_widths_still_carry_a_uint_constant() {
    const SRC: &str = "fun b(v: UByte): String = v.toString()\n\
fun s(v: UShort): String = v.toString()\n\
fun i(v: UInt): String = v.toString()\n\
fun l(v: ULong): String = v.toString()\n\
fun box(): String {\n\
    if (b(200u) != \"200\") return \"fail ubyte: \" + b(200u)\n\
    if (s(40000u) != \"40000\") return \"fail ushort: \" + s(40000u)\n\
    if (i(4294967295u) != \"4294967295\") return \"fail uint: \" + i(4294967295u)\n\
    if (l(3u) != \"3\") return \"fail ulong: \" + l(3u)\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}
