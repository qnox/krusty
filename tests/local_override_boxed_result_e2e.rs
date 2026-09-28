//! A primitive override of a declaration whose result is not primitive returns the wrapper, as
//! kotlinc's signature mapper makes it: `echo(x: Int): Int` over `Echo<T>.echo(x: T): T` is
//! `echo(I)Ljava/lang/Integer;`, and `invoke` of a `() -> Boolean` object is
//! `invoke()Ljava/lang/Boolean;`. The body boxes its result once, a call through the class
//! (a `super` call included) unboxes it, and the erased bridge returns the box as it is.
//! `module_override_boxed_result_e2e.rs` covers a public class and its callers in other files.

use super::common;

const SRC: &str = "interface Echo<T> { fun echo(x: T): T }\n\
    fun box(): String {\n\
    \x20   open class Next : Echo<Int> { override fun echo(x: Int): Int = x + 1 }\n\
    \x20   class Last : Next() { override fun echo(x: Int): Int = super.echo(x) + 10 }\n\
    \x20   val yes = object : () -> Boolean { override fun invoke(): Boolean = true }\n\
    \x20   val next = Next()\n\
    \x20   if (next.echo(1) * 2 != 4) return \"direct\"\n\
    \x20   val last: Echo<Int> = Last()\n\
    \x20   if (last.echo(1) != 12) return \"bridge\"\n\
    \x20   val function: () -> Boolean = yes\n\
    \x20   if (!function()) return \"object\"\n\
    \x20   return \"OK\"\n\
    }\n";

fn assert_same_instructions(class: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "LocalOverrideBoxedResult",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits {class}.{member}");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference,
        "{class}.{member}"
    );
}

#[test]
fn a_local_override_returns_the_wrapper_like_kotlinc() {
    let class = "LocalOverrideBoxedResultKt$box$Next";
    assert_same_instructions(class, "public java.lang.Integer echo(int);");
    assert_same_instructions(class, "public java.lang.Object echo(java.lang.Object);");
}

#[test]
fn a_super_call_unboxes_the_wrapper_like_kotlinc() {
    assert_same_instructions(
        "LocalOverrideBoxedResultKt$box$Last",
        "public java.lang.Integer echo(int);",
    );
}

#[test]
fn an_object_function_returns_the_wrapper_like_kotlinc() {
    let class = "LocalOverrideBoxedResultKt$box$yes$1";
    assert_same_instructions(class, "public java.lang.Boolean invoke();");
    assert_same_instructions(class, "public java.lang.Object invoke();");
}

#[test]
fn a_call_through_the_class_unboxes_like_kotlinc() {
    assert_same_instructions(
        "LocalOverrideBoxedResultKt",
        "public static final java.lang.String box();",
    );
}

#[test]
fn boxed_override_results_run() {
    common::expect_box_ok_with_stdlib(SRC, "LocalOverrideBoxedResult");
}
