//! A reified member extension publishes its call-site type arguments, so inlining substitutes
//! them into `typeOf<T>()`.
use super::common::{self, kotlinc_box_files_result};

fn agree(sources: &[(&str, &str)]) {
    let krusty = common::compile_and_run_files_with_stdlib(sources).expect("krusty box");
    let kotlinc = kotlinc_box_files_result(sources, "MainKt");
    assert_eq!(krusty, kotlinc);
    assert_eq!(krusty, "OK");
}

#[test]
fn a_reified_member_extension_substitutes_type_of() {
    agree(&[
        (
            "lib.kt",
            r#"
import kotlin.reflect.typeOf

class A {
    inline fun <reified T : CharSequence> A.foo(a: T) = typeOf<T>()
}
"#,
        ),
        (
            "main.kt",
            r#"
import kotlin.reflect.typeOf

fun box(): String {
    if (A().run { foo("0123456789") } != typeOf<String>()) return "FAIL"
    return "OK"
}
"#,
        ),
    ]);
}

#[test]
fn a_reified_member_extension_with_a_context_parameter_substitutes_type_of() {
    agree(&[
        (
            "lib.kt",
            r#"
import kotlin.reflect.typeOf

class A {
    context(c: Int)
    inline fun <reified T : CharSequence> A.foo(a: T) = typeOf<T>()
}
"#,
        ),
        (
            "main.kt",
            r#"
import kotlin.reflect.typeOf

fun box(): String = with(42) {
    if (A().run { foo("0123456789") } != typeOf<String>()) return "FAIL"
    return "OK"
}
"#,
        ),
    ]);
}
