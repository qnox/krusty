//! Override matching substitutes the implementation's own type parameters by the inherited ones.
//!
//! A class declared inside the body of the inherited generic function can name that function's
//! type parameter directly. kotlinc's override checker then sees the inherited formal in the
//! inherited slot and accepts the override (KT-9152); a class-level type parameter with the same
//! spelling is a different identity and still overrides nothing.

use super::common;

#[test]
fn a_local_class_may_name_the_inherited_function_formal() {
    let source = "var result = \"FAIL\"\n\
        open class A {\n\
        \x20   open fun <T> f(x: T) {\n\
        \x20       class B : A() {\n\
        \x20           override fun <S> f(x: T) { result = x as String }\n\
        \x20       }\n\
        \x20       return B().f<T>(x)\n\
        \x20   }\n\
        }\n\
        fun box(): String { A().f(\"OK\"); return result }\n";
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(source),
        Vec::<String>::new()
    );
}

#[test]
fn a_class_type_parameter_is_not_the_inherited_function_formal() {
    let source = "open class C { open fun <T> f(x: T) {} }\n\
        class D<T> : C() { override fun <S> f(x: T) {} }\n";
    let result = common::compiler_diagnostics(&[("D.kt", source)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "class type parameter in the inherited slot");
}
