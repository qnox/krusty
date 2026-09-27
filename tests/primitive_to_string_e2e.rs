//! kotlinc's `FlattenStringConcatenationLowering` turns `toString()` on a primitive receiver into a
//! one-argument string concatenation, which `JvmStringConcatenationLowering` realizes as
//! `String.valueOf` overloaded by the primitive (`(J)`, `(I)` for `Byte`/`Short`, …), marking the
//! call's line. A callable reference's adapter body calls it the same way.
use super::common;

const SOURCE: &str = "package store\n\
    \n\
    fun sink(s: String) {}\n\
    fun wide(x: Int): Long = x.toLong()\n\
    \n\
    fun ints(x: Int): String = x.toString()\n\
    fun longs(x: Int): String = wide(x).toString()\n\
    fun bytes(x: Byte): String = x.toString()\n\
    fun shorts(x: Short): String = x.toString()\n\
    fun chars(x: Char): String = x.toString()\n\
    fun booleans(x: Boolean): String = x.toString()\n\
    fun floats(x: Float): String = x.toString()\n\
    fun doubles(x: Double) {\n\
    \x20   sink(x.toString())\n\
    }\n\
    fun nullable(x: Int?): String = x.toString()\n\
    fun reference(): (Int) -> String = Int::toString\n\
    fun bound(x: Long): () -> String = x::toString\n";

#[test]
fn primitive_to_string_is_byte_identical_to_kotlinc() {
    for class in [
        "store/PrimitiveToStringKt",
        "store/PrimitiveToStringKt$reference$1",
        "store/PrimitiveToStringKt$bound$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "PrimitiveToString",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|difference| panic!("{class}: {difference}"));
    }
}
