//! A value class's private member is realized as a private static `-impl`; its companion (another
//! JVM class) calls it through kotlinc's `public static final synthetic access$<name>-impl` bridge
//! over the same parameters, since a cross-class private `invokestatic` fails with
//! `IllegalAccessError`.

use super::common;

const SRC: &str = "@JvmInline value class Weight(private val grams: Int) {\n\
    \x20   private fun heavy(): String = if (grams > 10) \"heavy\" else \"OK\"\n\
    \x20   companion object {\n\
    \x20       fun check(weight: Weight): String = weight.heavy()\n\
    \x20   }\n\
    }\n\
    fun box(): String = Weight.check(Weight(0))\n";

fn assert_same_method(class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc("Main", &[], SRC, class, method) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("{class}.{method}: reference toolchain unavailable"),
    }
}

#[test]
fn the_private_impl_gets_a_static_access_bridge() {
    assert_same_method(
        "Weight",
        "public static final java.lang.String access$heavy-impl(int)",
    );
}

#[test]
fn the_companion_calls_the_bridge() {
    assert_same_method("Weight$Companion", "public final java.lang.String check-");
}

#[test]
fn a_companion_calls_a_private_value_class_member() {
    let output = common::compile_and_run_box(SRC, "Main", &[common::stdlib_jar()], None)
        .expect("krusty compiles and the JVM runs the box function");
    assert_eq!(output, "OK");
}
