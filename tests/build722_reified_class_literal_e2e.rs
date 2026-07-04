//! build.722 (ee1 foundation): a class literal on a REIFIED type parameter — `T::class` inside an
//! `inline fun <reified T>`. krusty rejected it outright ("unresolved reference 'T'" / "class-literal form
//! is not supported") because `class_literal_unbound_ty` returns `None` for any type parameter.
//!
//! The checker now accepts `T::class` when `T` is a REIFIED type parameter (a non-reified one still errors,
//! as kotlinc rejects it), recording it as an unbound class literal; the lowerer, expanding the inline body
//! with `reified_subst` bound to the call-site type argument, substitutes `T` to that concrete type and
//! emits its class constant (`Prov::class` → `ldc Prov.class`).
use super::common;

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    common::compile_and_run_box(main, "Main", &[sl, jdk.clone()], Some(&jdk))
}

#[test]
fn reified_class_literal_simple_name() {
    const MAIN: &str =
        "inline fun <reified T: Any> nameOf(): String = T::class.simpleName ?: \"?\"\n\
        fun box(): String {\n\
        \x20 val n = nameOf<String>()\n\
        \x20 return if (n == \"String\") \"OK\" else \"fail: $n\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("reified T::class.simpleName"), "OK");
}

#[test]
fn reified_class_literal_custom_type() {
    // A user type as the reified argument.
    const MAIN: &str = "class Widget\n\
        inline fun <reified T: Any> nameOf(): String = T::class.simpleName ?: \"?\"\n\
        fun box(): String {\n\
        \x20 val n = nameOf<Widget>()\n\
        \x20 return if (n == \"Widget\") \"OK\" else \"fail: $n\"\n\
        }\n";
    assert_eq!(run(MAIN).expect("reified T::class of a user type"), "OK");
}
