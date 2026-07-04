//! An unqualified member-function reference `::m` inside a class is a BOUND reference to the enclosing
//! receiver — `this::m`. It captures `this` and lowers to the same `FunctionReferenceImpl` as `obj::m`.
//! Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn implicit_this_member_function_reference() {
    const SRC: &str = "class A {\n\
    var r = \"\"\n\
    fun mf() { r += \"O\" }\n\
    fun test(): String {\n\
        val f = ::mf\n\
        f()\n\
        r += \"K\"\n\
        return r\n\
    }\n\
}\n\
fun box(): String = A().test()\n";
    assert_eq!(run(SRC).expect("::memberFn = this::memberFn"), "OK");
}

#[test]
fn implicit_this_member_function_with_arg() {
    const SRC: &str = "class B {\n\
    fun add(s: String): String = \"O\" + s\n\
    fun test(): String = (::add)(\"K\")\n\
}\n\
fun box(): String = B().test()\n";
    assert_eq!(run(SRC).expect("::memberFn with arg"), "OK");
}
