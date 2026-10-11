//! Line numbers around an unsigned bitwise `constructor-impl` match kotlinc.
//!
//! Those members are inline-only. kotlinc drops the callee's own line numbers and, after the
//! body, either writes the caller's line again (inside a condition, on the jump) or forgets it so
//! the next mark of that line survives: a literal argument, the store, or the return. A literal
//! argument owns the line when the receiver is a plain local of a declaration or expression. A
//! condition keeps the line on the first instruction, and an assignment keeps it on the receiver
//! load and writes it again at the store. A nested assignment keeps the inner literal on that
//! same receiver line; the outer literal is marked after the inner `constructor-impl`, and the
//! store after the outer one. An assignment whose right operand is itself unsigned records
//! that line twice at the receiver load. A loop condition keeps an unsigned test's line on the
//! receiver load. The other argument, when it is a plain local, records
//! no line of its own.

use super::common;

fn expect_class_matches(name: &str, src: &str, class: &str) {
    match common::class_bytes_diff_against_kotlinc(name, &[], src, class, "class") {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const LINES: &str = "fun g(): UInt = 1u\n\
\n\
fun shrStore(v: UInt): UInt {\n\
    var remaining = v shr 7\n\
    return remaining\n\
}\n\
\n\
fun andOr(v: UInt): UInt {\n\
    val byte = (v and 0x7fu) or 0x80u\n\
    return byte\n\
}\n\
\n\
fun nested(result: UInt, cur: UInt, count: Int): UInt {\n\
    val out = result or ((cur and 0x7fu) shl (count * 7))\n\
    return out\n\
}\n\
\n\
fun multiline(v: UInt, x: UInt): UInt {\n\
    return v and\n\
        x\n\
}\n\
\n\
fun expr(v: UInt) = v shr 7\n\
\n\
fun bothLocals(a: UInt, b: UInt) = a or b\n\
\n\
fun andShl(v: UInt, c: Int) = (v and 0x7fu) shl c\n\
\n\
fun callRhs(v: UInt): UInt {\n\
    return v or\n\
        g()\n\
}\n\
\n\
fun inIf(v: UInt): Int {\n\
    if (v shr 1 == 0u) return 1\n\
    return 0\n\
}\n\
\n\
fun toU(b: Byte): UInt {\n\
    val cur = b.toUInt() and 0xffu\n\
    return cur\n\
}\n\
\n\
fun write(v: UInt, writeNextByte: (Byte) -> Unit) {\n\
    @Suppress(\"NAME_SHADOWING\")\n\
    var v = v\n\
    var remaining = v shr 7\n\
    while (remaining != 0u) {\n\
        val byte = (v and 0x7fu) or 0x80u\n\
        writeNextByte(byte.toByte())\n\
        v = remaining\n\
        remaining = remaining shr 7\n\
    }\n\
    val byte = v and 0x7fu\n\
    writeNextByte(byte.toByte())\n\
}\n\
\n\
fun nestedAssignment(a: UInt): UInt {\n\
    var x = a\n\
    x = (x and 0x7fu) or 0x80u\n\
    return x\n\
}\n\
\n\
fun orLocal(result: UInt, cur: UInt): UInt {\n\
    var r = result\n\
    r = r or cur\n\
    return r\n\
}\n\
\n\
fun orAnd(result: UInt, cur: UInt): UInt {\n\
    var r = result\n\
    r = r or (cur and 0x7fu)\n\
    return r\n\
}\n\
\n\
fun orShl(result: UInt, cur: UInt, count: Int): UInt {\n\
    var r = result\n\
    r = r or (cur shl count)\n\
    return r\n\
}\n\
\n\
fun readU(readNextByte: () -> Byte): UInt {\n\
    var result = 0u\n\
    var cur: UInt\n\
    var count = 0\n\
    do {\n\
        cur = readNextByte().toUInt() and 0xffu\n\
        result = result or ((cur and 0x7fu) shl (count * 7))\n\
        count++\n\
    } while (cur and 0x80u == 0x80u && count <= 4)\n\
    return result\n\
}\n";

#[test]
fn unsigned_bitwise_lines_match_kotlinc() {
    expect_class_matches("UnsignedBitwiseLines", LINES, "UnsignedBitwiseLinesKt");
}
