//! `is`/`!is` against a function type — the named `FunctionN<…>` form (`x is Function1<*, *>`) and its
//! nullable variant, plus nullable-primitive `is`. Lowers to `instanceof kotlin/jvm/functions/FunctionN`.
mod common;
use common::assert_box_ok_with_stdlib;

#[test]
fn is_function_type_named() {
    let src = r#"
fun box(): String {
    val f: Any? = { x: Int -> x + 1 }
    if (f !is Function1<*, *>) return "fail: !is Function1"
    if (f is Function0<*>) return "fail: is Function0"
    val g: Any? = 5
    if (g is Function1<*, *>) return "fail: Int is Function1"
    return "OK"
}
"#;
    assert_box_ok_with_stdlib(src, "is_function_type_named");
}

#[test]
fn is_nullable_function_and_primitive() {
    let src = r#"
fun box(): String {
    val n: Any? = null
    if (n !is Int?) return "fail: null !is Int?"
    if (n !is Function1<*, *>?) return "fail: null !is Function?"
    if (n is Int) return "fail: null is Int"
    if (n is Function1<*, *>) return "fail: null is Function"
    val fv: Any? = { x: Int -> x + 1 }
    if (fv !is Function1<*, *>?) return "fail: fv !is Function?"
    if (fv is String?) return "fail: fv is String?"
    return "OK"
}
"#;
    assert_box_ok_with_stdlib(src, "is_nullable_function_and_primitive");
}
