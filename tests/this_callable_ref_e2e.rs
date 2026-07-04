//! Bound callable references with a `this` receiver: `this::method` (a function value) and `this::prop`
//! (`KProperty0`). The lowering already captures `this`=value 0; this covers the resolver typing.
//! Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn this_method_ref_passed_to_hof() {
    const SRC: &str = "fun apply1(f: (Int) -> Int, x: Int): Int = f(x)\n\
class Calc(val base: Int) {\n\
    fun add(x: Int): Int = base + x\n\
    fun run(): Int = apply1(this::add, 10)\n\
}\n\
fun box(): String = if (Calc(32).run() == 42) \"OK\" else \"fail\"\n";
    let out = run(SRC).expect("this::method passed to a HOF should compile + run");
    assert_eq!(out, "OK");
}
