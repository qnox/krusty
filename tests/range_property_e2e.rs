//! Range-typed top-level/local properties (`val r = 1..10`) infer their stdlib range type and can be
//! iterated/used. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "P", &[sl], Some(&jdk))
}

#[test]
fn toplevel_range_property_iterates() {
    const SRC: &str = "val r = 1..5\n\
val cr = 'a'..'c'\n\
fun box(): String {\n\
    var sum = 0\n\
    for (x in r) sum += x\n\
    if (sum != 15) return \"fail sum: $sum\"\n\
    var s = \"\"\n\
    for (c in cr) s += c\n\
    if (s != \"abc\") return \"fail chars: $s\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("range-typed property should compile + run");
    assert_eq!(out, "OK");
}
