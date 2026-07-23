//! Top-level properties initialized from unsigned literals (`val ua = 1234U`) infer `UInt`/`ULong`.
//! Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "P")
}

#[test]
fn toplevel_unsigned_literal_property() {
    // Corpus unsignedTypes/... shape.
    const SRC: &str = "val ua = 1234U\n\
val ub = 1U\n\
fun box(): String {\n\
    if (ua + ub != 1235U) return \"fail add\"\n\
    if (ua == ub) return \"fail eq\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("top-level unsigned literal property should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn unsigned_string_concat_and_ulong_promotion() {
    // `"x" + uint` must print the UNSIGNED value; a `U` literal exceeding UInt.MAX is a ULong.
    const SRC: &str = "fun box(): String {\n\
    val s = \"INT \" + 0x8fffffffU\n\
    if (s != \"INT 2415919103\") return \"fail int: \" + s\n\
    val l = \"LONG \" + 0xffff_ffff_ffffU\n\
    if (l != \"LONG 281474976710655\") return \"fail long: \" + l\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("unsigned string concat should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn unsigned_tostring_uses_unsigned_semantics() {
    // Corpus unsignedTypes/unsignedIntToString.kt + unsignedLongToString.kt: `.toString()` on an
    // unsigned literal must render the UNSIGNED value, not the signed carrier.
    const SRC: &str = "fun box(): String {\n\
    val min = 0U.toString()\n\
    if (\"0\" != min) return \"fail min: \" + min\n\
    val middle = 2_147_483_647U.toString()\n\
    if (\"2147483647\" != middle) return \"fail mid: \" + middle\n\
    val max = 4_294_967_295U.toString()\n\
    if (\"4294967295\" != max) return \"fail max: \" + max\n\
    val lmax = 18_446_744_073_709_551_615UL.toString()\n\
    if (\"18446744073709551615\" != lmax) return \"fail lmax: \" + lmax\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("unsigned toString should compile + run");
    assert_eq!(out, "OK");
}
