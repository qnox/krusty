//! A spliced lambda's result is boxed from the carrier its own block declares.
//!
//! A stdlib `inline fun` is spliced as bytecode and its lambda argument emitted inline at the
//! `FunctionN.invoke` site it replaces. That `invoke` returns `Object`, so a primitive result must be
//! boxed on the way out, and the splicer decides that from the body's physical carrier type.
//!
//! A lambda holding a labelled `return@map` carries its result through a local its own block
//! declares. Value indices are numbered per body, so resolving that local through the ambient slot
//! and variable tables answered with whichever body last claimed the number — the enclosing caller,
//! since the lambda is emitted into a scratch frame. For an `Int` body it reported `Iterable`, the
//! scalar test failed, and the box was skipped entirely:
//!
//! ```text
//! java.lang.VerifyError: Bad type on operand stack
//!   Type integer (current frame, stack[1]) is not assignable to 'java/lang/Object'
//! ```
//!
//! krusty reported SUCCESS and wrote a class the JVM refuses to load, so only RUNNING the result
//! catches it. The declaration is in the block itself and is not ambiguous, so the block now answers
//! for it.
//!
//! The carrier must stay PHYSICAL here: `semantic_scalar_adapter` treats it as the authority for
//! whether a value is physically scalar, and an unsigned value class is semantically unsigned but
//! physically `int`. `tests/unsigned_generic_erasure_e2e.rs` is the oracle for that distinction.

use super::common;

/// The failing shape: a labelled return inside `map`, with a PRIMITIVE element type.
#[test]
fn a_labeled_return_in_map_boxes_a_primitive_result() {
    const MAIN: &str = "fun mapped(keys: List<String>, skip: Boolean): List<Int> =\n\
\x20   keys.map { key ->\n\
\x20       if (skip) return@map 0\n\
\x20       key.length\n\
\x20   }\n\
fun box(): String {\n\
\x20   if (mapped(listOf(\"ab\", \"c\"), false) != listOf(2, 1)) return \"FAIL: kept\"\n\
\x20   if (mapped(listOf(\"ab\", \"c\"), true) != listOf(0, 0)) return \"FAIL: skipped\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "map_labeled_return_boxing");
}

/// Every primitive carrier takes the same route, and a `Long` occupies two slot words — a wrong
/// carrier width would corrupt the frame rather than merely skip a box.
#[test]
fn every_primitive_element_carrier_is_boxed() {
    const MAIN: &str = "fun longs(keys: List<String>, skip: Boolean): List<Long> =\n\
\x20   keys.map { key -> if (skip) return@map 0L; key.length.toLong() }\n\
fun flags(keys: List<String>, skip: Boolean): List<Boolean> =\n\
\x20   keys.map { key -> if (skip) return@map false; key.isNotEmpty() }\n\
fun chars(keys: List<String>, skip: Boolean): List<Char> =\n\
\x20   keys.map { key -> if (skip) return@map 'x'; key[0] }\n\
fun box(): String {\n\
\x20   if (longs(listOf(\"ab\"), false) != listOf(2L)) return \"FAIL: long kept\"\n\
\x20   if (longs(listOf(\"ab\"), true) != listOf(0L)) return \"FAIL: long skipped\"\n\
\x20   if (flags(listOf(\"ab\"), false) != listOf(true)) return \"FAIL: boolean\"\n\
\x20   if (chars(listOf(\"ab\"), true) != listOf('x')) return \"FAIL: char\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "primitive_element_boxing");
}

/// An UNSIGNED element keeps its semantic box: the carrier stays physical, so the adapter still
/// picks `UInt`'s wrapper rather than `Integer`. This is the distinction the fix must not blur.
#[test]
fn an_unsigned_element_keeps_its_semantic_box() {
    const MAIN: &str = "fun codes(keys: List<String>, skip: Boolean): List<UInt> =\n\
\x20   keys.map { key -> if (skip) return@map 0u; key.length.toUInt() }\n\
fun box(): String {\n\
\x20   if (codes(listOf(\"ab\"), false) != listOf(2u)) return \"FAIL: kept\"\n\
\x20   if (codes(listOf(\"ab\"), true) != listOf(0u)) return \"FAIL: skipped\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "unsigned_element_boxing");
}

/// A non-local `return` still leaves the enclosing function for real, and a reference element still
/// needs no box — neither decision may change.
#[test]
fn non_local_returns_and_reference_elements_are_unchanged() {
    const MAIN: &str = "fun shortOnly(keys: List<String>): List<Int> {\n\
\x20   return keys.map { key ->\n\
\x20       if (key.length > 3) return emptyList()\n\
\x20       key.length\n\
\x20   }\n\
}\n\
fun labels(keys: List<String>, skip: Boolean): List<String> =\n\
\x20   keys.map { key -> if (skip) return@map \"none\"; key }\n\
fun plain(keys: List<String>): List<Int> = keys.map { it.length }\n\
fun box(): String {\n\
\x20   if (shortOnly(listOf(\"ab\")) != listOf(2)) return \"FAIL: local\"\n\
\x20   if (shortOnly(listOf(\"abcd\")) != emptyList<Int>()) return \"FAIL: non-local\"\n\
\x20   if (labels(listOf(\"a\"), true) != listOf(\"none\")) return \"FAIL: reference\"\n\
\x20   if (plain(listOf(\"ab\")) != listOf(2)) return \"FAIL: plain\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "unchanged_lambda_exits");
}
