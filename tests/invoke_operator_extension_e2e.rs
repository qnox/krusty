//! An `operator fun Recv.invoke(...)` EXTENSION makes `recv(args)` call it (`"a"(12)` →
//! `invoke("a", 12)`). Lowered as `invokestatic <facade>.invoke(recv, args)`. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn invoke_extension_on_string_literal() {
    const SRC: &str = "operator fun String.invoke(i: Int) = \"$this$i\"\n\
fun box() = if (\"a\"(12) == \"a12\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("String.invoke extension"), "OK");
}

#[test]
fn invoke_extension_on_user_type() {
    const SRC: &str = "class V(val n: Int)\n\
operator fun V.invoke(d: Int): Int = n + d\n\
fun box(): String {\n\
    val v = V(40)\n\
    return if (v(2) == 42) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("user-type invoke extension"), "OK");
}
