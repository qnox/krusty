//! Interface-method DEFAULT ARGUMENTS: an `interface` method with a default-valued parameter, called
//! through an implementor while omitting that argument (`FooImpl().foo()`). kotlinc emits a static
//! `Foo.foo$default(Foo, params, int mask, Object marker)` that applies the defaults then dispatches to
//! the real method; a caller omitting defaults invokes it. Verified by running `box()` on a real JVM.
mod common;
fn run(src: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    let jdk = std::env::var("JAVA_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|jh| std::path::PathBuf::from(format!("{jh}/lib/modules")));
    let cp = std::slice::from_ref(&stdlib);
    let classes = common::compile_in_process(src, "T", cp, jdk.as_deref())?;
    common::run_box(&classes, "TKt", cp)
}
#[test]
fn interface_method_default_arg_omitted_and_supplied() {
    let src = "interface Foo { fun foo(a: Double = 1.0): Double }\n\
        class FooImpl : Foo { override fun foo(a: Double): Double = a }\n\
        fun box(): String {\n\
        \x20 if (FooImpl().foo() != 1.0) return \"omit\"\n\
        \x20 if (FooImpl().foo(2.0) != 2.0) return \"supply\"\n\
        \x20 return \"OK\"\n}\n";
    match run(src) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => panic!("compile/run returned None (feature missing or no toolchain)"),
    }
}
