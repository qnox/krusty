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

/// Run one fixture under BOTH compilers and require the same `box()` value.
///
/// `expect_box_ok_files_with_stdlib` alone only proves krusty agrees with itself: it compiles and
/// runs krusty's output and checks for `OK`. These shapes are about matching the reference
/// compiler's carrier decisions, so the reference must run the identical source.
fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main)], stem);
}

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
    both_compilers_box(MAIN, "map_labeled_return_boxing");
}

/// ALL EIGHT primitive carriers take the same route. `Long` and `Double` occupy two slot words, so a
/// wrong carrier width corrupts the frame rather than merely skipping a box; `Byte`, `Short` and
/// `Char` are the narrow cases whose boxed and unboxed spellings differ.
#[test]
fn every_primitive_element_carrier_is_boxed() {
    const MAIN: &str = "fun bytes(keys: List<String>, skip: Boolean): List<Byte> =\n\
\x20   keys.map { key -> if (skip) return@map 0; key.length.toByte() }\n\
fun shorts(keys: List<String>, skip: Boolean): List<Short> =\n\
\x20   keys.map { key -> if (skip) return@map 0; key.length.toShort() }\n\
fun ints(keys: List<String>, skip: Boolean): List<Int> =\n\
\x20   keys.map { key -> if (skip) return@map 0; key.length }\n\
fun longs(keys: List<String>, skip: Boolean): List<Long> =\n\
\x20   keys.map { key -> if (skip) return@map 0L; key.length.toLong() }\n\
fun floats(keys: List<String>, skip: Boolean): List<Float> =\n\
\x20   keys.map { key -> if (skip) return@map 0.0f; key.length.toFloat() }\n\
fun doubles(keys: List<String>, skip: Boolean): List<Double> =\n\
\x20   keys.map { key -> if (skip) return@map 0.0; key.length.toDouble() }\n\
fun flags(keys: List<String>, skip: Boolean): List<Boolean> =\n\
\x20   keys.map { key -> if (skip) return@map false; key.isNotEmpty() }\n\
fun chars(keys: List<String>, skip: Boolean): List<Char> =\n\
\x20   keys.map { key -> if (skip) return@map 'x'; key[0] }\n\
fun box(): String {\n\
\x20   val one = listOf(\"ab\")\n\
\x20   if (bytes(one, false) != listOf<Byte>(2)) return \"FAIL: byte kept\"\n\
\x20   if (bytes(one, true) != listOf<Byte>(0)) return \"FAIL: byte skipped\"\n\
\x20   if (shorts(one, false) != listOf<Short>(2)) return \"FAIL: short kept\"\n\
\x20   if (shorts(one, true) != listOf<Short>(0)) return \"FAIL: short skipped\"\n\
\x20   if (ints(one, false) != listOf(2)) return \"FAIL: int kept\"\n\
\x20   if (ints(one, true) != listOf(0)) return \"FAIL: int skipped\"\n\
\x20   if (longs(one, false) != listOf(2L)) return \"FAIL: long kept\"\n\
\x20   if (longs(one, true) != listOf(0L)) return \"FAIL: long skipped\"\n\
\x20   if (floats(one, false) != listOf(2.0f)) return \"FAIL: float kept\"\n\
\x20   if (floats(one, true) != listOf(0.0f)) return \"FAIL: float skipped\"\n\
\x20   if (doubles(one, false) != listOf(2.0)) return \"FAIL: double kept\"\n\
\x20   if (doubles(one, true) != listOf(0.0)) return \"FAIL: double skipped\"\n\
\x20   if (flags(one, false) != listOf(true)) return \"FAIL: boolean kept\"\n\
\x20   if (flags(one, true) != listOf(false)) return \"FAIL: boolean skipped\"\n\
\x20   if (chars(one, false) != listOf('a')) return \"FAIL: char kept\"\n\
\x20   if (chars(one, true) != listOf('x')) return \"FAIL: char skipped\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "primitive_element_boxing");
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
    both_compilers_box(MAIN, "unsigned_element_boxing");
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
    both_compilers_box(MAIN, "unchanged_lambda_exits");
}
