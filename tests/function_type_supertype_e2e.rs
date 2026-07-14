//! A class/object may declare a FUNCTION TYPE as a supertype (`class C : () -> R`), implementing the
//! JVM functional interface `kotlin/jvm/functions/FunctionN`. The class provides `override fun invoke`,
//! and an instance is assignable to the matching `(…) -> R` and callable as a function value. Covers
//! nullary, parameterised, `object`, and extension-receiver (`Recv.() -> R`) forms. Runs on the JVM.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nullary_function_supertype_called_as_value() {
    const SRC: &str = "class C : () -> String {\n\
        \x20 override fun invoke(): String = \"OK\"\n\
        }\n\
        fun box(): String {\n\
        \x20 val f: () -> String = C()\n\
        \x20 return f()\n\
        }\n";
    assert_eq!(run(SRC).expect("nullary fn supertype"), "OK");
}

#[test]
fn parameterised_function_supertype() {
    const SRC: &str = "class Add : (Int, Int) -> Int {\n\
        \x20 override fun invoke(a: Int, b: Int): Int = a + b\n\
        }\n\
        fun box(): String {\n\
        \x20 val f: (Int, Int) -> Int = Add()\n\
        \x20 return if (f(2, 3) == 5) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("param fn supertype"), "OK");
}

#[test]
fn object_function_supertype() {
    const SRC: &str = "object Greet : () -> String {\n\
        \x20 override fun invoke(): String = \"OK\"\n\
        }\n\
        fun box(): String {\n\
        \x20 val g: () -> String = Greet\n\
        \x20 return g()\n\
        }\n";
    assert_eq!(run(SRC).expect("object fn supertype"), "OK");
}

#[test]
fn function_type_as_type_argument_is_not_a_function_supertype() {
    // Regression: a generic supertype whose type ARGUMENT is a function type (`Base<() -> String>`)
    // must NOT be misread as a function-type supertype — the `->` sits inside `<…>`, at depth > 0.
    const SRC: &str = "open class Base<T>\n\
        class C : Base<() -> String>() {\n\
        \x20 fun ok(): String = \"OK\"\n\
        }\n\
        fun box(): String = C().ok()\n";
    assert_eq!(run(SRC).expect("fn type as type arg"), "OK");
}

#[test]
fn extension_receiver_function_supertype() {
    // `Recv.() -> R` folds the receiver into the first `FunctionN` parameter (`Function1<Recv, R>`),
    // so the class implements `Function1` with an `invoke(Recv)` (calling it via receiver syntax
    // `5.h()` is a separate function-value-invoke path — here the interface method is called directly).
    const SRC: &str = "class Ext : Int.() -> Int {\n\
        \x20 override fun invoke(x: Int): Int = x + 1\n\
        }\n\
        fun box(): String {\n\
        \x20 val h = Ext()\n\
        \x20 return if (h.invoke(5) == 6) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("extension-receiver fn supertype"), "OK");
}
