//! A local a non-inlined lambda captures and writes lives in a `Ref$XxxRef` holder. kotlinc's
//! `JvmSharedVariablesManager.defineSharedValue` sets the new holder's `element` only when the
//! initializer is not a constant equal to the element type's default: `var r = 0` creates the holder
//! and stores nothing, since the field already holds `0`. krusty stored every initializer, so its
//! class files carried an extra `iconst_0; putfield` per such local. The comparison is Kotlin's
//! boxed equality (`-0.0` is stored), a nullable element's default is `null` (`var r: Int? = 0` is
//! stored), and an unsigned element is a value class whose `0u` is stored too.

use super::common;

const DECLARATIONS: &str = "fun run(f: () -> Unit) = f()\n\
fun zero(): Int { var r = 0; run { r += 1 }; return r }\n\
fun five(): Int { var r = 5; run { r += 1 }; return r }\n\
fun none(): String? { var r: String? = null; run { r = \"a\" }; return r }\n\
fun no(): Boolean { var r = false; run { r = !r }; return r }\n\
fun zeroLong(): Long { var r = 0L; run { r += 1L }; return r }\n\
fun zeroDouble(): Double { var r = 0.0; run { r += 1.0 }; return r }\n\
fun negativeZero(): Double { var r = -0.0; run { r *= 2.0 }; return r }\n\
fun zeroFloat(): Float { var r = 0f; run { r += 1f }; return r }\n\
fun negativeZeroFloat(): Float { var r = -0f; run { r *= 2f }; return r }\n\
fun nul(): Char { var r = '\\u0000'; run { r = 'a' }; return r }\n\
fun zeroByte(): Byte { var r: Byte = 0; run { r = 1 }; return r }\n\
fun zeroShort(): Short { var r: Short = 0; run { r = 1 }; return r }\n\
fun boxedZero(): Int? { var r: Int? = 0; run { r = r?.plus(1) }; return r }\n\
fun unsignedZero(): UInt { var r = 0u; run { r = 1u }; return r }\n\
fun unsignedLongZero(): ULong { var r = 0uL; run { r = 1uL }; return r }\n\
fun empty(): String { var r = \"\"; run { r += \"x\" }; return r }\n\
fun assignedLater(): Int { var r: Int; r = 0; run { r += 1 }; return r }\n\
fun computed(x: Int): Int { var r = x - x; run { r += 1 }; return r }\n";

#[test]
fn a_default_constant_initializer_sets_no_holder_element_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SharedCellDefaults",
        DECLARATIONS,
        &["SharedCellDefaultsKt"],
    );
}

#[test]
fn a_holder_created_without_its_default_initializer_reads_that_default() {
    let source = format!(
        "{DECLARATIONS}\
fun inlined(): Int {{ var r = 0; repeat(2) {{ r += 1 }}; return r }}\n\
fun box(): String {{\n\
    val r = listOf(zero(), five(), none(), no(), zeroLong(), zeroDouble(), negativeZero(), zeroFloat(),\n\
        negativeZeroFloat(), nul(), zeroByte(), zeroShort(), boxedZero(), unsignedZero(), unsignedLongZero(),\n\
        empty(), assignedLater(), computed(3), inlined()).joinToString()\n\
    return if (r == \"1, 6, a, true, 1, 1.0, -0.0, 1.0, -0.0, a, 1, 1, 1, 1, 1, x, 1, 1, 2\") \"OK\" else r\n\
}}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "SharedCellDefaultsRun");
}
