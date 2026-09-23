//! A data class whose property is a value class declared in a SIBLING file.
//!
//! The generated members over such a property call the value class's static `-impl` functions over
//! the underlying (`Tag.hashCode-impl(I)I`), exactly as for a value class in the same file. Emission
//! asked `IrFile` whether the property's class is a value class, and for a class another file declares
//! the answer came from a table nothing had filled since the AST lowerer was removed. The property
//! then read as an ordinary object and `hashCode` dispatched on the unboxed carrier:
//!
//! ```text
//! VerifyError: Holder.hashCode()I: Type integer (current frame, stack[0]) is not assignable to 'Tag'
//! ```
//!
//! The value-class inventory now publishes every value class it resolves from outside the file.
use super::common;

const TAG: &str = "@JvmInline\n\
    value class Tag(val value: Int)\n";

const HOLDER: &str = "data class Holder(val count: Tag)\n\
    fun make(n: Int) = Holder(Tag(n))\n";

#[test]
fn a_sibling_value_class_property_drives_the_data_class_members() {
    const MAIN: &str = "fun box(): String {\n\
        \x20   val holder = make(3)\n\
        \x20   if (holder.hashCode() != make(3).hashCode()) return \"FAIL hashCode\"\n\
        \x20   if (holder.hashCode() != Tag(3).hashCode()) return \"FAIL hashCode value\"\n\
        \x20   if (holder != make(3)) return \"FAIL equals\"\n\
        \x20   if (holder == make(4)) return \"FAIL not equals\"\n\
        \x20   if (holder.toString() != \"Holder(count=Tag(value=3))\") return \"FAIL toString \" + holder\n\
        \x20   val (count) = holder.copy(count = Tag(5))\n\
        \x20   return if (count.value == 5) \"OK\" else \"FAIL copy \" + count\n\
        }\n";
    common::expect_box_ok_files_with_stdlib(
        &[("Tag.kt", TAG), ("Holder.kt", HOLDER), ("Main.kt", MAIN)],
        "a data class over a sibling-file value class",
    );
}
