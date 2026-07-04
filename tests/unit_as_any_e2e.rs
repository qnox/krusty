//! `Unit` is a subtype of `Any` — a `Unit` value used where `Any`/`Any?` is expected materializes the
//! `kotlin/Unit` singleton (the expression runs for effect, then `Unit.INSTANCE` is pushed). Round-tripped.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unit_returned_as_any() {
    const SRC: &str = "fun bar() {}\n\
fun foo(): Any? = bar()\n\
fun box(): String {\n\
    val x: Any? = foo()\n\
    if (x != Unit) return \"fail 1\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit returned as Any"), "OK");
}

#[test]
fn unit_stored_in_any_array() {
    const SRC: &str = "fun bar() {}\n\
fun box(): String {\n\
    val a = arrayOfNulls<Any>(1)\n\
    a[0] = bar()\n\
    return if (a[0] == Unit) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("Unit stored in Array<Any>"), "OK");
}

#[test]
fn unit_passed_as_any_arg() {
    const SRC: &str = "fun bar() {}\n\
fun id(x: Any?): Any? = x\n\
fun box(): String = if (id(bar()) == Unit) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("Unit passed as Any arg"), "OK");
}
