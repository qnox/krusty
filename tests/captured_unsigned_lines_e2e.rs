//! Line numbers for an unsigned operation whose receiver is not a plain local.
//!
//! A property read and a captured mutable local keep the line they wrote. The literal does not
//! take another entry. An assignment into the captured local writes the line again at the store.
//! A declaration of a captured local keeps the line on the zero default unboxing plants and on
//! the element store, not on the initializer's literal. A plain local still lets the literal own
//! the line; `toUInt()` still does, because the inlined call forgets.

use super::common;

const SOURCE: &str = "class Holder(var v: UInt)\n\
    \n\
    fun prop(b: Holder): UInt {\n\
    \x20   val byte = b.v and 0x7fu\n\
    \x20   return byte\n\
    }\n\
    \n\
    fun nestedProp(b: Holder): UInt {\n\
    \x20   val byte = (b.v and 0x7fu) or 0x80u\n\
    \x20   return byte\n\
    }\n\
    \n\
    fun maskCaptured(v0: UInt): UInt {\n\
    \x20   var v = v0\n\
    \x20   repeat(1) { v = v and 0x7fu }\n\
    \x20   return v\n\
    }\n\
    \n\
    fun captured(v0: UInt, writeNextByte: (Byte) -> Unit) {\n\
    \x20   var v = v0\n\
    \x20   var remaining = v shr 7\n\
    \x20   repeat(UInt.SIZE_BYTES) {\n\
    \x20       val byte = (v and 0x7fu) or 0x80u\n\
    \x20       writeNextByte(byte.toByte())\n\
    \x20       v = remaining\n\
    \x20       remaining = remaining shr 7\n\
    \x20   }\n\
    \x20   val byte = v and 0x7fu\n\
    \x20   writeNextByte(byte.toByte())\n\
    }\n";

#[test]
fn captured_and_property_unsigned_lines_match_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "CapturedUnsignedLines",
        SOURCE,
        "CapturedUnsignedLinesKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|difference| panic!("{difference}"));
}
