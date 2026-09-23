//! A data class whose property is a value class declared in a SIBLING file.
//!
//! The generated members over such a property call the value class's static `-impl` functions over
//! the underlying (`SiblingCount.hashCode-impl(I)I`), exactly as for a value class in the same file.
//! Emission asked `IrFile` whether the property's class is a value class, and for a class another
//! file declares the answer came from a table nothing had filled since the AST lowerer was removed.
//! The property then read as an ordinary object and `hashCode` dispatched on the unboxed carrier:
//!
//! ```text
//! VerifyError: SiblingEnvelope.hashCode()I: Type integer (current frame, stack[0]) is not
//! assignable to 'SiblingCount'
//! ```
//!
//! The checked classifier-fact path now publishes the sibling declaration before emission.
use super::common;

const COUNT: &str = "package fixtures.siblingdata\n\
    @JvmInline\n\
    value class SiblingCount(val value: Int)\n";

const ENVELOPE: &str = "package fixtures.siblingdata\n\
    data class SiblingEnvelope(val count: SiblingCount)\n\
    fun makeSiblingEnvelope(n: Int) = SiblingEnvelope(SiblingCount(n))\n";

#[test]
fn a_sibling_value_class_property_drives_the_data_class_members() {
    const MAIN: &str = "package fixtures.siblingdata\n\
        fun box(): String {\n\
        \x20   val holder = makeSiblingEnvelope(3)\n\
        \x20   if (holder.hashCode() != makeSiblingEnvelope(3).hashCode()) return \"FAIL hashCode\"\n\
        \x20   if (holder.hashCode() != SiblingCount(3).hashCode()) return \"FAIL hashCode value\"\n\
        \x20   if (holder != makeSiblingEnvelope(3)) return \"FAIL equals\"\n\
        \x20   if (holder == makeSiblingEnvelope(4)) return \"FAIL not equals\"\n\
        \x20   if (holder.toString() != \"SiblingEnvelope(count=SiblingCount(value=3))\") return \"FAIL toString \" + holder\n\
        \x20   val (count) = holder.copy(count = SiblingCount(5))\n\
        \x20   return if (count.value == 5) \"OK\" else \"FAIL copy \" + count\n\
        }\n";
    let sources = [
        ("SiblingCount.kt", COUNT),
        ("SiblingEnvelope.kt", ENVELOPE),
        ("Main.kt", MAIN),
    ];
    common::expect_box_ok_files_with_stdlib(
        &sources,
        "a data class over a sibling-file value class",
    );
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "fixtures.siblingdata.MainKt"),
        "OK",
        "kotlinc reference for a data class over a sibling-file value class",
    );
}
