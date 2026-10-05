//! A value class whose underlying type is a type parameter bounded by another value class erases
//! through that bound to the innermost carrier, whether it is declared in the file that uses it or
//! in a sibling file of the same module. An inlined lambda taking it as a parameter unboxes the
//! `invoke` argument to that carrier. The inline function is this repository's own, compiled by
//! the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <T, R> T.myLet(block: (T) -> R): R = block(this)
"#;

const CLASSES: &str = r#"
@JvmInline
value class Inner<T : Long>(val l: T)

@JvmInline
value class Outer<T : Inner<Long>>(val o: T) {
    fun simple(): String = if (o.l == 5L) "OK" else "FAIL ${o.l}"
}
"#;

const MAIN: &str = r#"
fun box(): String = Outer(Inner(5L)).myLet { it.simple() }
"#;

#[test]
fn a_sibling_value_class_over_a_bounded_value_class_reaches_an_inlined_lambda() {
    let lib =
        common::kotlinc_lib_out(&[("Lib.kt", LIB)]).expect("reference kotlinc is provisioned");
    let jdk = common::jdk_modules();
    let out = common::compile_and_run_box_files(
        &[("Classes.kt", CLASSES), ("Main.kt", MAIN)],
        &[lib, common::stdlib_jar()],
        Some(jdk.as_path()),
    );
    assert_eq!(out.as_deref(), Some("OK"));
}
