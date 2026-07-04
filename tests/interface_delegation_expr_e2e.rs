//! Interface delegation to an EXPRESSION (`class D : I by Impl()`), not just a `val` constructor
//! parameter. The expression is evaluated once and stored in a synthesized `$$delegate` field; each of
//! `I`'s methods forwards to it. Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "P")
}

fn toolchain_ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

#[test]
fn delegate_to_constructor_call_expression() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
interface Greeter { fun greet(): String }\n\
class Impl : Greeter { override fun greet() = \"OK\" }\n\
class D : Greeter by Impl()\n\
fun box(): String = D().greet()\n";
    assert_eq!(
        run(SRC).expect("delegation to an expression should compile + run"),
        "OK"
    );
}

#[test]
fn delegate_expression_referencing_constructor_param() {
    if !toolchain_ready() {
        return;
    }
    // The delegate expression is evaluated in the constructor, so it may reference a ctor parameter.
    const SRC: &str = "// WITH_STDLIB\n\
interface Greeter { fun greet(): String }\n\
class Impl(val s: String) : Greeter { override fun greet() = s }\n\
fun mk(s: String): Greeter = Impl(s)\n\
class D(val x: String) : Greeter by mk(x)\n\
fun box(): String = D(\"OK\").greet()\n";
    assert_eq!(
        run(SRC).expect("delegate referencing ctor param should compile + run"),
        "OK"
    );
}

#[test]
fn delegate_expression_with_class_body() {
    if !toolchain_ready() {
        return;
    }
    // The `{ … }` after `by Impl()` is the CLASS BODY, not a trailing lambda on the delegate call.
    const SRC: &str = "// WITH_STDLIB\n\
interface Greeter { fun greet(): String }\n\
class Impl : Greeter { override fun greet() = \"O\" }\n\
class D : Greeter by Impl() {\n\
    fun extra() = \"K\"\n\
}\n\
fun box(): String = D().greet() + D().extra()\n";
    assert_eq!(
        run(SRC).expect("delegate with class body should compile + run"),
        "OK"
    );
}
