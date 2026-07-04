//! A lambda value inside a class member (method, `init`, property initializer) is synthesized as a
//! closure whose impl method lives ON the enclosing class and captures the enclosing `this` as its
//! first parameter — so it can read instance fields and call instance methods. Previously any value
//! lambda in a class member was rejected. Round-tripped on the JVM (the captured `this` reads real
//! state; a `var` it mutates is `Ref`-boxed; a vararg member call wraps its argument).

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn lambda_captures_instance_field() {
    const SRC: &str = "class C(val v: String) {\n\
        fun make(): () -> String = { v }\n\
    }\n\
    fun box(): String = C(\"OK\").make()()\n";
    assert_eq!(run(SRC).expect("lambda capturing a field"), "OK");
}

#[test]
fn lambda_calls_instance_method() {
    const SRC: &str = "class C(val a: String, val b: String) {\n\
        private fun combine() = a + b\n\
        fun make(): () -> String = { combine() }\n\
    }\n\
    fun box(): String = C(\"O\", \"K\").make()()\n";
    assert_eq!(run(SRC).expect("lambda calling a private method"), "OK");
}

#[test]
fn require_with_message_lambda_in_init() {
    const SRC: &str = "@JvmInline\n\
    value class Name(val value: String) {\n\
        init { require(value.isNotBlank()) { \"name is blank\" } }\n\
    }\n\
    fun box(): String = Name(\"OK\").value\n";
    assert_eq!(run(SRC).expect("require with message-lambda in init"), "OK");
}

#[test]
fn non_this_lambda_in_base_ctor_arg() {
    // A lambda in a base-constructor argument runs PRE-`super()`, where `this` is not available — it
    // must NOT capture `this` (it doesn't use it).
    const SRC: &str = "open class A(val n: Int)\n\
    class B : A({ 1 + 2 }())\n\
    fun box(): String = if (B().n == 3) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("non-this lambda in base-ctor arg"), "OK");
}

#[test]
fn closure_mutates_var_in_property_init() {
    // A `var` declared in a property initializer and mutated by a (non-inline) closure is `Ref`-boxed.
    const SRC: &str = "fun bar(b: () -> Unit) { b() }\n\
    class C { val p: Int = run { var v = 10; bar { v = 20 }; v + 1 } }\n\
    fun box(): String = if (C().p == 21) \"OK\" else \"fail ${C().p}\"\n";
    assert_eq!(
        run(SRC).expect("closure mutating a var in a property init"),
        "OK"
    );
}

#[test]
fn vararg_member_call_in_closure() {
    // A vararg member call inside a closure wraps its single argument into the array parameter.
    const SRC: &str = "class C {\n\
        fun base(vararg s: String): Int = s.size\n\
        fun make(): (String) -> Int = { base(it) }\n\
    }\n\
    fun box(): String = if (C().make()(\"x\") == 1) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("vararg member call inside a closure"), "OK");
}
