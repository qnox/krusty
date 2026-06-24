//! Top-level properties initialized from unsigned literals (`val ua = 1234U`) infer `UInt`/`ULong`.
//! Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "P", &[sl], Some(&jdk))
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
