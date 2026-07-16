//! A LOCAL function with default parameters may be called with the trailing defaulted arguments
//! omitted (`fun bar(x: Int = 1); bar()`). krusty emits local functions as plain methods (no `$default`
//! synthetic), so the omitted defaults are filled at the call site.
mod common;
use common::assert_box_ok_with_stdlib;

#[test]
fn local_fun_omitted_defaults() {
    let src = r#"
fun box(): String {
    fun f(a: Int, b: Int = 10, c: String = "z") = "$a$b$c"
    if (f(1) != "110z") return "fail: f(1)"
    if (f(1, 2) != "12z") return "fail: f(1,2)"
    if (f(1, 2, "q") != "12q") return "fail: f(1,2,q)"
    fun g(x: Int = 5) = x * 2
    if (g() != 10) return "fail: g()"
    if (g(3) != 6) return "fail: g(3)"
    return "OK"
}
"#;
    assert_box_ok_with_stdlib(src, "local_fun_omitted_defaults");
}
