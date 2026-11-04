//! Line numbers around an unsigned bitwise `constructor-impl`, and around an inline-only unsigned
//! operator whose arguments are read in place, match kotlinc.
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
//!
//! An operator such as `plus` is inlined with its arguments read in place. kotlinc generates them
//! before the call marks its line and then moves their code, line labels included, to where the
//! body loads them; a declaration or a `return` marks its line only once its value is on the
//! stack. In `fun f(x: UInt) = x + 1u` the receiver is the caller's local, the literal marks the
//! line first, and the call's own mark repeats it, so the entry sits on the literal. Two local
//! operands mark nothing, so the line stays where the body begins. An assignment, a condition and
//! an intrinsic such as `toString()` mark their line before their operand and keep it there.

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

const OPERATOR_LINES: &str = "fun g(): UInt = 1u\n\
\n\
fun expressionBody(x: UInt) = x + 1u\n\
\n\
fun returned(x: UInt): UInt {\n\
    return x + 1u\n\
}\n\
\n\
fun declared(x: UInt): UInt {\n\
    val y = x + 1u\n\
    return y\n\
}\n\
\n\
fun nextLine(x: UInt) =\n\
    x + 1u\n\
\n\
fun operatorOnNextLine(x: UInt) = x +\n\
    1u\n\
\n\
fun explicitCall(x: UInt) = x.plus(1u)\n\
\n\
fun callArgument(x: UInt) = x + g()\n\
\n\
fun ulong(x: ULong) = x * 10uL\n\
\n\
fun literalReceiver(x: UInt) = 1u + x\n\
\n\
fun bothLocals(x: UInt, y: UInt) = x * y\n\
\n\
fun conversion(x: UInt) = x.toLong()\n\
\n\
fun assigned(x: UInt): UInt {\n\
    var y = x\n\
    y = y + 2u\n\
    return y\n\
}\n\
\n\
fun compound(x: UInt): UInt {\n\
    var y = x\n\
    y += 5u\n\
    return y\n\
}\n\
\n\
fun condition(x: UInt): Int {\n\
    if (x + 1u == 0u) return 1\n\
    return 0\n\
}\n\
\n\
fun rendered(x: UInt) = (x + 1u).toString()\n\
\n\
fun element(xs: List<UInt>) = xs[0] + 1u\n";

#[test]
fn unsigned_operator_lines_match_kotlinc() {
    expect_class_matches(
        "UnsignedOperatorLines",
        OPERATOR_LINES,
        "UnsignedOperatorLinesKt",
    );
}
