//! A deferred declaration reading a member function whose RETURN is itself inferred.
//!
//! `class W<T>(val value: T) { fun get() = value }` / `val x = W("OK").get()`: member returns were
//! pre-inferred only after signature collection returned, so while `x` was being typed `get`'s
//! return was still undetermined and the read erased to the parameter's bound — `x` published `Any`.
//! The same call inside a function body was always right, because by then the pre-inference had run.
//!
//! Delegates are the shape that made it visible: `val O by W("OK")` resolves the property's type
//! through `getValue`, so an undetermined return there loses the delegate's element type entirely.
use super::common;

#[test]
fn a_declaration_reads_an_inferred_member_return() {
    const SRC: &str = "class W<T>(val value: T) { fun get() = value }\n\
        val x = W(\"OK\").get()\n\
        fun box(): String = x\n";
    common::expect_box_ok_with_stdlib(SRC, "DeferredInferredMemberReturn");
}

#[test]
fn a_delegate_resolves_through_an_inferred_get_value() {
    const SRC: &str = "import kotlin.reflect.KProperty\n\
        class W<T>(val value: T) { operator fun getValue(t: Any?, p: KProperty<*>) = value }\n\
        object A { val O by W(\"OK\") }\n\
        fun box(): String = A.O\n";
    common::expect_box_ok_with_stdlib(SRC, "DeferredInferredGetValue");
}

#[test]
fn a_provide_delegate_carries_the_lambda_result() {
    // The corpus shape: `provideDelegate` returns a delegate whose element type comes from a lambda
    // the constructor was given, so both the member return and the lambda's result must survive.
    const SRC: &str = "import kotlin.reflect.KProperty\n\
        class W<T>(val value: T) { operator fun getValue(t: Any?, p: KProperty<*>) = value }\n\
        class N<T>(val build: (String) -> T) {\n\
        \x20   operator fun provideDelegate(t: Any?, p: KProperty<*>) = W(build(p.name))\n\
        }\n\
        object A { val O by N { it + \"K\" } }\n\
        fun box(): String = A.O\n";
    common::expect_box_ok_with_stdlib(SRC, "DeferredProvideDelegate");
}
