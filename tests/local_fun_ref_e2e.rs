//! A local function reference `::localFun` is a closure over the function's lifted static method. A
//! capturing local function captures the same outer locals the lifted method takes as leading params,
//! so `::loc` (where `loc` reads an enclosing `val`) carries that capture into the closure. Round-tripped
//! under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn local_function_reference_with_and_without_capture() {
    const SRC: &str = "fun apply1(f: (Int) -> Int, v: Int) = f(v)\n\
fun box(): String {\n\
    fun inc(x: Int) = x + 1\n\
    if (apply1(::inc, 4) != 5) return \"fail inc\"\n\
    val base = 100\n\
    fun shift(x: Int) = x + base\n\
    if (apply1(::shift, 4) != 104) return \"fail capture: \" + apply1(::shift, 4)\n\
    val g = ::inc\n\
    if (g(9) != 10) return \"fail g\"\n\
    val xs = listOf(1, 2, 3)\n\
    if (xs.map(::shift) != listOf(101, 102, 103)) return \"fail map\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("local function reference should compile + run");
    assert_eq!(out, "OK");
}
