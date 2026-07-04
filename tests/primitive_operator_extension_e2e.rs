//! A user `operator fun` extension on a PRIMITIVE receiver whose argument type is NOT a builtin
//! operand (`operator fun Int.times(v: V): V`). The builtin `Int.times` only applies to a numeric
//! argument, so `2 * V(3)` / `2.times(V(3))` must resolve to the extension — not be rejected as an
//! unsupported builtin operator. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn primitive_receiver_operator_extension_infix() {
    const SRC: &str = "data class V(val x: Int)\n\
operator fun Int.times(v: V): V = V(this * v.x)\n\
fun box(): String = if ((2 * V(3)).x == 6) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("Int * V extension operator"), "OK");
}

#[test]
fn primitive_receiver_operator_extension_dot() {
    const SRC: &str = "data class V(val x: Int)\n\
operator fun Int.plus(v: V): V = V(this + v.x)\n\
fun box(): String = if (2.plus(V(40)).x == 42) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("Int.plus(V) extension operator"), "OK");
}
