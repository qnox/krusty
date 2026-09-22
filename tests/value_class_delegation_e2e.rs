//! `value class IC(val i: I) : I by i` — a value class delegating through its own underlying value.
//!
//! A value class has exactly ONE field, and this shape names that very field as the delegate.
//! Kotlin synthesizes no `$$delegate_0` there — the underlying value IS the delegate, and every
//! forwarder reads it — and it could not: a second field is not a shape a value class has.
//!
//! Common lowering synthesized one anyway, which gave the class two fields. The JVM emitter turned
//! that into a `putfield` of the wrong type in `constructor-impl` and the class was rejected at load
//! with `VerifyError: Bad type on operand stack in putfield`; the native code generator refused the
//! class by name. kotlinc compiles every program here.
//!
//! WHICH field the delegate is cannot be decided where the delegation field used to be created —
//! the property's own field does not exist yet at that point — so the edge is recorded later, where
//! the constructor's field indices are known.

use super::common;

const UNDERLYING_DELEGATE_SOURCE: &str = "// LANGUAGE: +InlineClassImplementationByDelegation\n\
     interface I {\n\
     \x20   fun ok(): String\n\
     }\n\
     @JvmInline\n\
     value class IC(val i: I) : I by i\n\
     fun box(): String {\n\
     \x20   val i = object : I {\n\
     \x20       override fun ok(): String = \"OK\"\n\
     \x20   }\n\
     \x20   if (IC(i).ok() != \"OK\") return \"fail through the value class\"\n\
     \x20   val boxed: I = IC(i)\n\
     \x20   return boxed.ok()\n\
     }\n";

/// Run `body` under krusty AND under the reference compiler, and require the SAME output.
fn agrees_with_kotlinc(stem: &str, body: &str) {
    let krusty = common::expect_box_run_with_stdlib(body, stem);
    let reference = common::kotlinc_box_result(body);
    assert_eq!(reference, "OK", "{stem}: unexpected kotlinc result");
    assert_eq!(krusty, reference, "{stem}: krusty and kotlinc disagree");
}

/// Reached through the value class's own type and through the interface it delegates.
#[test]
fn a_value_class_delegates_through_its_underlying_value() {
    agrees_with_kotlinc("ValueClassDelegates", UNDERLYING_DELEGATE_SOURCE);
}

/// Runtime verification catches the old invalid `putfield`, while this pins the underlying cause:
/// the value class must have exactly kotlinc's one field, with no unused `$$delegate_0` surviving.
#[test]
fn a_value_class_reuses_the_exact_underlying_field_layout() {
    let classes =
        common::expect_classes_with_stdlib(UNDERLYING_DELEGATE_SOURCE, "ValueClassDelegateFields");
    let (_, bytes) = classes
        .iter()
        .find(|(name, _)| name == "IC")
        .expect("krusty emits IC");
    let ours = krusty::jvm::classreader::parse_class(bytes).expect("krusty IC parses");

    let reference = common::kotlinc_library(UNDERLYING_DELEGATE_SOURCE)
        .expect("reference compiler emits value-class delegation fixture");
    let reference = std::fs::read(reference.join("IC.class")).expect("read kotlinc IC");
    let reference = krusty::jvm::classreader::parse_class(&reference).expect("kotlinc IC parses");

    assert_eq!(reference.fields.len(), 1, "kotlinc fixture changed shape");
    assert_eq!(ours.fields, reference.fields, "IC field layout differs");
}

/// The underlying value is still READABLE as the property it is: the delegation does not take the
/// field away from it.
#[test]
fn the_underlying_value_is_still_its_own_property() {
    agrees_with_kotlinc(
        "ValueClassDelegateProperty",
        "// LANGUAGE: +InlineClassImplementationByDelegation\n\
         interface I {\n\
         \x20   fun tag(): String\n\
         }\n\
         class Real(val text: String) : I {\n\
         \x20   override fun tag() = text\n\
         }\n\
         @JvmInline\n\
         value class IC(val i: I) : I by i\n\
         fun box(): String {\n\
         \x20   val ic = IC(Real(\"OK\"))\n\
         \x20   if (ic.tag() != \"OK\") return \"fail forwarder\"\n\
         \x20   val underlying = ic.i\n\
         \x20   if (underlying.tag() != \"OK\") return \"fail underlying\"\n\
         \x20   return if (underlying === ic.i) \"OK\" else \"fail identity\"\n\
         }\n",
    );
}

/// A GENERIC underlying type, which is the other half of the corpus's cases.
#[test]
fn a_generic_value_class_delegates_the_same_way() {
    agrees_with_kotlinc(
        "ValueClassDelegatesGeneric",
        "// LANGUAGE: +InlineClassImplementationByDelegation\n\
         interface I {\n\
         \x20   fun ok(): String\n\
         }\n\
         @JvmInline\n\
         value class IC<T : I>(val i: T) : I by i\n\
         fun box(): String {\n\
         \x20   val i = object : I {\n\
         \x20       override fun ok(): String = \"OK\"\n\
         \x20   }\n\
         \x20   val ic: I = IC(i)\n\
         \x20   return ic.ok()\n\
         }\n",
    );
}

/// An ORDINARY class delegating to a constructor parameter keeps its synthesized field: the shape
/// above is a value class's alone, and nothing about it reaches here.
#[test]
fn an_ordinary_class_still_delegates_through_a_field_of_its_own() {
    agrees_with_kotlinc(
        "OrdinaryClassDelegates",
        "interface I {\n\
         \x20   fun ok(): String\n\
         }\n\
         class Holder(private val first: I, private val second: I) : I by first {\n\
         \x20   fun other() = second.ok()\n\
         }\n\
         fun named(text: String): I = object : I {\n\
         \x20   override fun ok() = text\n\
         }\n\
         fun box(): String {\n\
         \x20   val holder = Holder(named(\"O\"), named(\"K\"))\n\
         \x20   return holder.ok() + holder.other()\n\
         }\n",
    );
}
