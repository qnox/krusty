//! Lambda arguments to a function-typed CONSTRUCTOR parameter get their parameter types from that
//! parameter's function type (`C({ x, y -> x + y })` — `x`/`y` are `Int`, not the erased `Any`), the
//! same inference a top-level call applies. Invoking a function-typed member PROPERTY (`obj.func(a, b)`)
//! reads the property and calls it through the invoke convention. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn lambda_ctor_arg_infers_param_types_and_member_invoke() {
    const SRC: &str = "class C(val func: (x: Int, y: Int) -> Int)\n\
fun box(): String = if (C({ x, y -> x + y }).func(2, 3) == 5) \"OK\" else \"no\"\n";
    assert_eq!(
        run(SRC).expect("ctor-lambda param inference + member-property invoke compiles + runs"),
        "OK"
    );
}

#[test]
fn enum_ctor_lambda_mutates_captured_var_across_nested_lambda() {
    // The enum entry's `() -> String` initializer mutates a captured `var` inside a further
    // (non-inline) lambda; the var must be Ref-boxed so the mutation is observed on return.
    const SRC: &str = "fun call(f: () -> Unit) { f() }\n\
enum class E(val f: () -> String) {\n\
    A({\n\
        var value = \"Fail\"\n\
        call { value = \"OK\" }\n\
        value\n\
    })\n\
}\n\
fun box(): String = E.A.f()\n";
    assert_eq!(
        run(SRC).expect("enum ctor lambda mutable capture compiles + runs"),
        "OK"
    );
}

#[test]
fn inner_class_captures_outer_in_super_ctor_lambda() {
    // An `inner class` passes a lambda capturing the outer instance's property to its `super(...)`
    // constructor argument; the captured value must reach the lambda without a VerifyError.
    const SRC: &str = "open class Base(val callback: () -> String)\n\
class Outer {\n\
    val ok = \"OK\"\n\
    inner class Inner : Base(run { val x = ok; { x } })\n\
}\n\
fun box(): String = Outer().Inner().callback()\n";
    assert_eq!(
        run(SRC).expect("inner-class outer capture in super ctor lambda compiles + runs"),
        "OK"
    );
}

#[test]
fn inner_super_ctor_direct_lambda_captures_outer_property() {
    // A direct lambda in an inner class's `super(...)` argument must capture the outer constructor
    // parameter, not the still-uninitialized inner `this`.
    const SRC: &str = "open class Base(val callback: () -> String)\n\
class Outer { val ok = \"OK\"\n inner class Inner : Base({ ok }) }\n\
fun box(): String = Outer().Inner().callback()\n";
    assert_eq!(
        run(SRC).expect("inner super ctor direct lambda captures outer property"),
        "OK"
    );
}

#[test]
fn top_level_super_ctor_nested_lambda_still_captures_property() {
    const SRC: &str = "open class Base(val callback: () -> String)\n\
val ok = \"OK\"\n\
class Sub : Base(run { val x = ok; { x } })\n\
fun box(): String = Sub().callback()\n";
    assert_eq!(
        run(SRC).expect("top-level property capture in nested ctor lambda compiles + runs"),
        "OK"
    );
}

#[test]
fn inner_super_ctor_inline_run_captures_outer_property() {
    const SRC: &str = "open class Base(val callback: String)\n\
class Outer { val ok = \"OK\"\n inner class Inner : Base(run { ok }) }\n\
fun box(): String = Outer().Inner().callback\n";
    assert_eq!(
        run(SRC).expect("inner super ctor inline run captures outer property"),
        "OK"
    );
}

#[test]
fn enum_ctor_lambda_arg_infers_param_types() {
    // A lambda passed to a function-typed enum-constructor parameter binds its parameter types from
    // that parameter's function type, then the entry's `func` property is invoked.
    const SRC: &str = "enum class Sign(val func: (x: Int, y: Int) -> Int) {\n\
    plus({ x, y -> x + y }),\n\
    mult({ x, y -> x * y })\n\
}\n\
fun box(): String {\n\
    val s = Sign.plus.func(2, 3)\n\
    val p = Sign.mult.func(4, 5)\n\
    return if (s == 5 && p == 20) \"OK\" else \"no: $s $p\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("enum ctor-lambda param inference compiles + runs"),
        "OK"
    );
}
