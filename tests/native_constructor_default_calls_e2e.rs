//! A constructor default that itself calls a function leaving arguments out.
//!
//! `class C(val n: Int = f())` where `fun f(x: Int = 7)` is two omissions: the construction's,
//! and the call its default makes. The default is code a constructor's default filling runs, so
//! the call inside it reaches the function's default-argument entry like any call in a body. The
//! same holds for a secondary constructor's default, an enum constant's, and a superclass
//! delegation's; a function default nested in a function default is the function-entry case.
//!
//! Every expectation is kotlinc's, taken by running the same program under it first.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the NATIVE answer to agree.
fn every_backend_agrees_with_kotlinc(stem: &str, source: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

/// A primary constructor's default calls a function with its own argument omitted.
#[test]
fn a_primary_constructor_default_calls_with_an_omission() {
    let source = "fun f(x: Int = 7) = x\n\
         class C(val n: Int = f())\n\
         fun box(): String = if (C().n == 7 && C(3).n == 3) \"OK\" else \"fail \" + C().n\n";
    every_backend_agrees_with_kotlinc("PrimaryDefaultCallsDefaulted", source);
}

/// A secondary constructor's default does the same.
#[test]
fn a_secondary_constructor_default_calls_with_an_omission() {
    let source = "fun f(x: Int = 7) = x\n\
         class C {\n\
         \x20   val n: Int\n\
         \x20   constructor(value: Int = f()) {\n\
         \x20       n = value\n\
         \x20   }\n\
         }\n\
         fun box(): String = if (C().n == 7) \"OK\" else \"fail \" + C().n\n";
    every_backend_agrees_with_kotlinc("SecondaryDefaultCallsDefaulted", source);
}

/// An enum constant leaving out an argument whose default calls with an omission.
#[test]
fn an_enum_constant_default_calls_with_an_omission() {
    let source = "fun f(x: Int = 7) = x\n\
         enum class E(val n: Int = f()) {\n\
         \x20   A,\n\
         \x20   B(3)\n\
         }\n\
         fun box(): String = if (E.A.n == 7 && E.B.n == 3) \"OK\" else \"fail \" + E.A.n\n";
    every_backend_agrees_with_kotlinc("EnumDefaultCallsDefaulted", source);
}

/// A superclass delegation leaving out an argument whose default calls with an omission.
#[test]
fn a_super_delegation_default_calls_with_an_omission() {
    let source = "fun f(x: Int = 7) = x\n\
         open class A(val n: Int = f())\n\
         class B : A()\n\
         fun box(): String = if (B().n == 7) \"OK\" else \"fail \" + B().n\n";
    every_backend_agrees_with_kotlinc("SuperDelegationDefaultCallsDefaulted", source);
}

/// A function default calling a function with an omission: the function-entry case.
#[test]
fn a_function_default_calls_with_an_omission() {
    let source = "fun f(x: Int = 7) = x\n\
         fun g(y: Int = f()) = y\n\
         fun box(): String = if (g() == 7 && g(3) == 3) \"OK\" else \"fail \" + g()\n";
    every_backend_agrees_with_kotlinc("FunctionDefaultCallsDefaulted", source);
}
