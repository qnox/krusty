//! `is` and `!is` checks of a type that admits `null`, and of `Nothing`. Checked FIR lowering
//! expands a nullable target into kotlinc's null-or-instance test and settles a `Nothing` target
//! to a constant; each lowered KLIB body is compared with checked FIR lowering of the equivalent
//! source, on hand-built trees and on the Kotlin/Native stdlib body that uses the form. A value
//! a test narrowed is used through the smart cast the declaring compilation states.

use super::super::*;
use super::demo_package::{demo, lowered_f, source_body, Demo, Shape, T};
use super::{distribution_root, frozen_function, lowered_body, stdlib, validated};
use crate::metadata::klib_ir::tree::{KlibIrExprKind, KlibIrStatement, KlibIrTypeOperator};
use crate::metadata::klib_ir::KlibIrConstant;
use crate::symbol_source::{SymbolNamespace, SymbolSource};

/// `f(a): Boolean`, `a` of the one type in `param`, returning `a <operator> target`.
fn type_test(operator: KlibIrTypeOperator, param: &'static [T], target: T) -> Demo {
    demo(&[("f", param, T::Boolean)], move |_, body| {
        let a = body.param(0);
        let operand = body.ty(target);
        let checked = body.expr(
            T::Boolean,
            KlibIrExprKind::TypeOperator {
                operator,
                operand,
                argument: a,
            },
        );
        Some(vec![body.ret(checked)])
    })
}

#[test]
fn a_nullable_type_test_of_a_stable_nullable_operand_lowers_as_the_source_test_does() {
    let cases = [
        (KlibIrTypeOperator::InstanceOf, "a is String?"),
        (KlibIrTypeOperator::NotInstanceOf, "a !is String?"),
    ];
    for (operator, expression) in cases {
        let lowered = lowered_f(&type_test(operator, &[T::NullableAny], T::NullableString));
        let source = format!("fun f(a: Any?): Boolean {{\n return {expression}\n}}\n");
        assert_eq!(lowered, source_body(&source, "f", 1), "{expression}");
    }
    // The stable parameter read is tested in place: `a == null || a is String`.
    assert_eq!(
        lowered_f(&type_test(
            KlibIrTypeOperator::InstanceOf,
            &[T::NullableAny],
            T::NullableString
        )),
        "scope {scope {return@Some(0) when[RefEq(v0, Null) -> Boolean(true), else -> \
         InstanceOf<String>(v0)]: Boolean: Nothing}: Nothing}"
    );
}

/// A generic operand whose bound admits `null` is tested as `kotlin.text.appendElement` tests
/// its `element: T` against `CharSequence?`.
#[test]
fn a_nullable_type_test_of_a_type_parameter_lowers_as_the_source_test_does() {
    let lowered = lowered_f(&type_test(
        KlibIrTypeOperator::InstanceOf,
        &[T::P],
        T::NullableString,
    ));
    let source = "fun <P> f(a: P): Boolean {\n return a is String?\n}\n";
    assert_eq!(lowered, source_body(source, "f", 1));
}

/// A non-null operand cannot be `null`, so the test is of the non-null target alone.
#[test]
fn a_nullable_type_test_of_a_non_null_operand_tests_the_non_null_target() {
    let lowered = lowered_f(&type_test(
        KlibIrTypeOperator::InstanceOf,
        &[T::Any],
        T::NullableString,
    ));
    let source = "fun f(a: Any): Boolean {\n return a is String?\n}\n";
    assert_eq!(lowered, source_body(source, "f", 1));
}

/// `Nothing` has no instances: `a is Nothing` evaluates `a` and answers `false`, and
/// `a is Nothing?` is `a == null`.
#[test]
fn a_nothing_type_test_settles_as_the_source_test_does() {
    let cases = [
        (KlibIrTypeOperator::InstanceOf, T::Nothing, "a is Nothing"),
        (
            KlibIrTypeOperator::NotInstanceOf,
            T::Nothing,
            "a !is Nothing",
        ),
        (
            KlibIrTypeOperator::InstanceOf,
            T::NullableNothing,
            "a is Nothing?",
        ),
    ];
    for (operator, target, expression) in cases {
        let lowered = lowered_f(&type_test(operator, &[T::NullableAny], target));
        let source = format!("fun f(a: Any?): Boolean {{\n return {expression}\n}}\n");
        assert_eq!(lowered, source_body(&source, "f", 1), "{expression}");
    }
}

/// An operand that is not a stable read is evaluated once into an `Any?` temporary.
#[test]
fn a_nullable_type_test_of_a_call_binds_its_value_once() {
    let shapes: [Shape; 2] = [
        ("g", &[T::NullableAny], T::NullableAny),
        ("f", &[T::NullableAny], T::Boolean),
    ];
    let negated_shapes = shapes;
    for (operator, expression) in [
        (KlibIrTypeOperator::InstanceOf, "g(a) is String?"),
        (KlibIrTypeOperator::NotInstanceOf, "g(a) !is String?"),
    ] {
        let demo = demo(&negated_shapes, move |name, body| {
            let a = body.param(0);
            if name == "g" {
                return Some(vec![body.ret(a)]);
            }
            let call = body.call("g", vec![a], T::NullableAny);
            let operand = body.ty(T::NullableString);
            let checked = body.expr(
                T::Boolean,
                KlibIrExprKind::TypeOperator {
                    operator,
                    operand,
                    argument: call,
                },
            );
            Some(vec![body.ret(checked)])
        });
        let mut unit = DependencyBodyUnit::default();
        let f = demo
            .lower(&mut unit, "f", &["g"])
            .unwrap_or_else(|decline| panic!("{decline}"));
        validated(unit.ir());
        let source = format!(
            "fun g(a: Any?): Any? {{\n return a\n}}\nfun f(a: Any?): Boolean {{\n \
             return {expression}\n}}\n"
        );
        assert_eq!(
            lowered_body(&unit, f),
            source_body(&source, "f", 1),
            "{expression}"
        );
    }
}

/// `fun f(a: <param>): <result> { if (a is <target>) return a; return <fallback> }`: the
/// returned `a` is the implicit cast of the value to the type its test proved, as
/// `appendElement` appends its `element: T` after `element is CharSequence?` and
/// `element is Char`.
fn smart_cast_after_test(param: &'static [T], target: T, result: T) -> Demo {
    demo(&[("f", param, result)], move |_, body| {
        let a = body.param(0);
        let tested = body.ty(target);
        let test = body.expr(
            T::Boolean,
            KlibIrExprKind::TypeOperator {
                operator: KlibIrTypeOperator::InstanceOf,
                operand: tested,
                argument: a,
            },
        );
        let a = body.param(0);
        let narrowed = body.type_operator(KlibIrTypeOperator::ImplicitCast, a, target);
        let returned = body.return_(narrowed);
        let guard = body.branches("IF", vec![(test, returned)], T::Unit);
        let fallback = match result {
            T::Int => body.int(0),
            T::String => body.string(""),
            _ => body.constant(T::NullableNothing, KlibIrConstant::Null),
        };
        Some(vec![KlibIrStatement::Expression(guard), body.ret(fallback)])
    })
}

#[test]
fn a_smart_cast_after_a_type_test_lowers_as_the_source_smart_cast_does() {
    /// The class model the calling compilation supplies: `String` is an `Any`.
    struct Classes;
    impl crate::assignable::TypeOracle for Classes {
        fn direct_supertypes(&self, ty: crate::types::Ty) -> Vec<crate::types::Ty> {
            if ty == crate::types::Ty::String {
                vec![crate::types::Ty::obj("kotlin/Any")]
            } else {
                Vec::new()
            }
        }
    }
    let classes = Classes;
    let facts = KlibCalleeFacts::default().with_type_oracle(&classes);
    let cases: [(&'static [T], T, T, &str); 4] = [
        (
            &[T::P],
            T::NullableString,
            T::NullableString,
            "fun <P> f(a: P): String? {\n if (a is String?) return a\n return null\n}\n",
        ),
        (
            &[T::P],
            T::String,
            T::String,
            "fun <P> f(a: P): String {\n if (a is String) return a\n return \"\"\n}\n",
        ),
        (
            &[T::P],
            T::Int,
            T::Int,
            "fun <P> f(a: P): Int {\n if (a is Int) return a\n return 0\n}\n",
        ),
        (
            &[T::NullableAny],
            T::NullableString,
            T::NullableString,
            "fun f(a: Any?): String? {\n if (a is String?) return a\n return null\n}\n",
        ),
    ];
    for (param, target, result, source) in cases {
        let demo = smart_cast_after_test(param, target, result);
        let mut unit = DependencyBodyUnit::default();
        let function = unit
            .lower_function(demo.callable("f"), &demo.bodies, &facts)
            .unwrap_or_else(|decline| panic!("{source}: {decline}"));
        validated(unit.ir());
        assert_eq!(
            with_type_parameter_named_p(&lowered_body(&unit, function)),
            with_type_parameter_named_p(&source_body(source, "f", 1)),
            "{source}"
        );
    }
}

/// `rendered` with each type-parameter identity spelled `P`. A KLIB body's `P` is identified by
/// its metadata declaration and the source's by its source declaration, so the two renderings
/// name the one type parameter differently.
fn with_type_parameter_named_p(rendered: &str) -> String {
    let mut normalized = String::new();
    let mut rest = rendered;
    while let Some(start) = rest.find("<\"\\0") {
        normalized.push_str(&rest[..start]);
        let identity = &rest[start..];
        let end = identity
            .find("\">")
            .expect("a closed type-parameter identity")
            + 2;
        normalized.push_str("<P>");
        rest = &identity[end..];
    }
    normalized.push_str(rest);
    normalized
}

/// `kotlin.text.appendElement`, which every `joinToString` reaches, tests its generic `element`
/// against `CharSequence?` before appending it. Lowered on its own, without facts for the
/// `Appendable` members it calls, the body now passes that test and stops at the first member
/// call instead.
#[test]
fn stdlib_append_element_lowers_past_its_nullable_type_test() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(crate::types::type_name("kotlin/text")),
        "appendElement",
    );
    let [append_element] = symbols.callables.functions() else {
        panic!("expected one appendElement declaration");
    };
    let fact = frozen_function(
        &libraries,
        "kotlin/text",
        "appendElement",
        append_element.semantic_receiver(),
        append_element.semantic_params().as_ref(),
    );
    let mut unit = DependencyBodyUnit::default();
    let decline = unit
        .lower_function(
            fact.klib_body_callable().expect("signed"),
            libraries.declaration_bodies(),
            &KlibCalleeFacts::default(),
        )
        .expect_err("the member calls have no callee facts");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `kotlin.text.appendElement` (it uses a call of a member function)"
    );
}
