//! Unsigned (`UInt`/`ULong`) value-class extensions resolve through `@Metadata` — their bytecode names
//! are `@JvmName`-mangled (`coerceAtMost-J1ME1BU`) and indexed under the erased underlying descriptor,
//! so resolution maps the Kotlin name + receiver via metadata, then calls the mangled method. The
//! signed-`Int` extension must NOT shadow it. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn uint_coerce_at_most() {
    const SRC: &str = "fun box(): String {\n\
    val a = 5u\n\
    val b = a.coerceAtMost(3u)\n\
    return if (b == 3u) \"OK\" else \"fail: \" + b\n\
}\n";
    let out = run(SRC).expect("UInt.coerceAtMost should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn uint_coerce_in() {
    const SRC: &str = "fun box(): String {\n\
    val r = 10u.coerceIn(1u, 5u)\n\
    return if (r == 5u) \"OK\" else \"fail: \" + r\n\
}\n";
    let out = run(SRC).expect("UInt.coerceIn should compile + run");
    assert_eq!(out, "OK");
}
