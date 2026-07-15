//! Regression: the function-type-supertype lookahead must stop at the closing `}` of the enclosing
//! class body. It previously decremented past a depth-0 `}` into the following declaration and
//! misread an unrelated `->` (a `when` arm or a lambda) as this supertype's function-type arrow,
//! which corrupted the class (its `Base()` constructor call was dropped) and made the file skip.

use crate::common;

#[test]
fn sealed_nested_class_before_when() {
    check("sealed/simple.kt");
}

fn check(rel: &str) {
    match common::run_box_corpus_case(rel) {
        Some(s) => assert_eq!(s, "OK", "{rel}"),
        None => panic!("unexpectedly skipped: {rel}"),
    }
}

#[test]
fn class_with_base_ctor_followed_by_when_expr_body() {
    let src = r#"
open class Base(val tag: String)

class Sub: Base("OK")

fun classify(n: Int): String = when (n) {
    0 -> "zero"
    else -> "other"
}

fun box(): String {
    val s = Sub()
    classify(1)
    return s.tag
}
"#;
    common::expect_box_ok_with_stdlib(src, "SupertypeScanArrow");
}
