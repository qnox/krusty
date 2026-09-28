//! A lambda whose function type takes or returns a value class over a non-null reference cannot be
//! built by `LambdaMetafactory`: its `invoke` takes the carrier where `FunctionN.invoke` takes the
//! box. kotlinc compiles such a lambda to a class of its own, `final` over `Object` and implementing
//! the function type, with the lambda's body as its specialized `invoke`, an erased bridge that
//! unboxes and boxes the value class, its captures as final synthetic fields, and a shared
//! `INSTANCE` when it captures nothing. A value class over a primitive keeps the indy lambda.

use super::common;

const SRC: &str = "@JvmInline value class Tag(val name: String)\n\
    @JvmInline value class Count(val n: Int)\n\
    fun label(f: (Tag) -> String, t: Tag): String = f(t)\n\
    fun echo(f: (Tag?) -> Tag?, t: Tag?): Tag? = f(t)\n\
    fun total(f: (Count) -> Int, c: Count): Int = f(c)\n\
    class Owner(private val secret: String) {\n\
    \x20   fun read(t: Tag): String = label({ secret }, t)\n\
    }\n\
    fun box(): String {\n\
    \x20   val a = label({ it.name }, Tag(\"O\"))\n\
    \x20   val b = echo({ it }, Tag(\"K\"))\n\
    \x20   val none = echo({ it }, null)\n\
    \x20   var suffix = \"\"\n\
    \x20   val c = label({ suffix = \"!\"; it.name }, Tag(\"-\"))\n\
    \x20   val d = total({ it.n + 1 }, Count(1))\n\
    \x20   val e = Owner(\"s\").read(Tag(\"x\"))\n\
    \x20   if (none != null || c != \"-\" || suffix != \"!\" || d != 2 || e != \"s\") return \"fail\"\n\
    \x20   return a + b!!.name\n\
    }\n";

/// kotlinc and krusty write `class` byte for byte alike.
fn assert_identical(class: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "ValueClassLambdaClass",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    assert!(!built.reference_bytes.is_empty(), "kotlinc writes {class}");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} differs from kotlinc's:\n{}\n---\n{}",
        built.krusty,
        built.reference
    );
}

#[test]
fn a_captureless_lambda_over_a_reference_value_class_is_a_singleton_class() {
    assert_identical("ValueClassLambdaClassKt$box$a$1");
}

#[test]
fn a_nullable_value_class_lambda_boxes_null_safely_in_its_bridge() {
    assert_identical("ValueClassLambdaClassKt$box$b$1");
    assert_identical("ValueClassLambdaClassKt$box$none$1");
}

#[test]
fn a_capturing_lambda_class_stores_its_shared_cell_in_a_field() {
    assert_identical("ValueClassLambdaClassKt$box$c$1");
}

#[test]
fn a_lambda_class_reads_its_owners_private_property_through_an_accessor() {
    assert_identical("Owner$read$1");
}

#[test]
fn lambda_classes_run() {
    common::expect_box_ok_with_stdlib(SRC, "ValueClassLambdaClass");
}
