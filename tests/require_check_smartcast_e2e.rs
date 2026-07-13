//! `require(x is T)` / `check(x is T)` are stdlib preconditions carrying the contract
//! `returns() implies (x is T)` — they throw when the condition is false, so `x` is smart-cast to `T`
//! for the rest of the block, exactly like an `if (x !is T) return` guard. krusty only narrowed on the
//! `if`-guard form, so a member access on `x` after `require(x is T)` failed to resolve. Round-tripped
//! on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn require_is_smartcasts_the_binding() {
    const SRC: &str = "open class Shape\nclass Circle(val r: Int) : Shape()\n\
fun area(x: Shape): Int {\n\
    require(x is Circle) { \"not a circle\" }\n\
    return x.r * x.r\n\
}\n\
fun box(): String = if (area(Circle(3)) == 9) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        run(SRC).expect("require(is) smartcast compiles + runs"),
        "OK"
    );
}

#[test]
fn check_is_smartcasts_the_binding() {
    const SRC: &str = "fun f(x: Any): Int {\n\
    check(x is String) { \"not a string\" }\n\
    return x.length\n\
}\n\
fun box(): String = if (f(\"hello\") == 5) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("check(is) smartcast compiles + runs"), "OK");
}
