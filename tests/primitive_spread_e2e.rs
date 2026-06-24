//! Single-spread of a PRIMITIVE array into a top-level `vararg` function (`f(*intArrayOf(1,2,3))`,
//! `f(*xs)` forwarding a vararg param): passed through `Arrays.copyOf(int[], int): int[]` with no
//! checkcast (the primitive overload returns the exact array type) — byte-identical to kotlinc. The
//! reference-array spread (`Object[]` copyOf + checkcast) was already supported. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn spread_int_array_literal() {
    const SRC: &str = "fun f(vararg xs: Int): Int = xs.sum()\n\
fun box(): String { val a = intArrayOf(1, 2, 3); return if (f(*a) == 6) \"OK\" else \"no\" }\n";
    assert_eq!(run(SRC).expect("primitive spread compiles + runs"), "OK");
}

#[test]
fn forward_vararg_param() {
    const SRC: &str = "fun f(vararg xs: Int): Int = xs.sum()\n\
fun g(vararg xs: Int): Int = f(*xs)\n\
fun box(): String = if (g(1, 2, 3) == 6) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("vararg-param forward compiles + runs"),
        "OK"
    );
}

#[test]
fn spread_long_array() {
    const SRC: &str = "fun f(vararg xs: Long): Long = xs.sum()\n\
fun box(): String { val a = longArrayOf(1L, 2L, 3L); return if (f(*a) == 6L) \"OK\" else \"no\" }\n";
    assert_eq!(run(SRC).expect("long spread compiles + runs"), "OK");
}
