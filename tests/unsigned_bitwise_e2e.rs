//! `UInt`/`ULong` bitwise members are the carrier opcode plus `constructor-impl`.
//!
//! kotlinc does not inline the stdlib bodies of infix `and`/`or`/`xor`/`shl`/`shr` or of `inv()`.
//! `shr` is the logical shift (`iushr`/`lushr`). The result is rebuilt with the value class's
//! `constructor-impl`, and the literal operand is the raw carrier (`bipush 127` for `0x7fu`).

use super::common;

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc(name, &[], src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const SRC: &str = "fun mask(v: UInt): UInt {\n\
    val remaining = v shr 7\n\
    val bits = (v and 0x7fu) or 0x80u\n\
    return bits xor remaining\n\
}\n\
\n\
fun maskLong(v: ULong): ULong {\n\
    val remaining = v shr 7\n\
    return (v and 0x7fuL) or remaining\n\
}\n\
\n\
fun shifted(v: UInt): UInt = v shl 3\n\
\n\
fun flipped(v: UInt): UInt = v.inv()\n\
\n\
fun flippedLong(v: ULong): ULong = v.inv()\n";

#[test]
fn unsigned_bitwise_members_match_kotlinc() {
    let class = "UnsignedBitwiseKt";
    for method in [
        "public static final int mask-WZ4Q5Ns(",
        "public static final long maskLong-VKZWuLQ(",
        "public static final int shifted-WZ4Q5Ns(",
        "public static final int flipped-WZ4Q5Ns(",
        "public static final long flippedLong-VKZWuLQ(",
    ] {
        expect_method_matches("UnsignedBitwise", SRC, class, method);
    }
}
