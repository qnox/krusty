//! How a local or anonymous class's `@Metadata` refers to a type parameter it captures from an
//! enclosing declaration, measured against kotlinc 2.4.20: by table id alone (`Type.type_parameter`),
//! never also by name. kotlinc numbers ids with a per-declaration interner: the class's own
//! parameters first, then each captured parameter on first use, with a member's first use numbered
//! in that member's own scope.

use super::common;

fn assert_identical(stem: &str, src: &str, class_internal: &str) {
    let classpath = [common::stdlib_jar()];
    common::metadata_diff_against_kotlinc_cp(stem, src, class_internal, &classpath)
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

const SRC: &str = "package app\n\
    \n\
    abstract class Base<T> {\n\
    \x20   abstract fun get(): T\n\
    }\n\
    \n\
    class Two<A, B>(val a: A, val b: B)\n\
    \n\
    fun <T> anon(t: T): Base<T> = object : Base<T>() {\n\
    \x20   override fun get(): T = t\n\
    }\n\
    \n\
    class Multi<E1, E2>(val a: E1, val b: E2) {\n\
    \x20   fun <X1, X2> wrap(x: X1, y: X2): Base<Two<E2, X1>> = object : Base<Two<E2, X1>>() {\n\
    \x20       override fun get(): Two<E2, X1> = Two(b, x)\n\
    \x20       fun other(): Two<X2, E1> = Two(y, a)\n\
    \x20   }\n\
    }\n";

#[test]
fn an_anonymous_object_addresses_a_captured_function_type_parameter_by_id() {
    assert_identical("CapturedFunction", SRC, "app/CapturedFunctionKt$anon$1");
}

#[test]
fn captured_type_parameters_are_numbered_on_first_use() {
    assert_identical("CapturedFirstUse", SRC, "app/Multi$wrap$1");
}
