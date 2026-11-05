//! Serialized parameter defaults, published as checked FIR lowering publishes a source
//! declaration's.

use super::super::DependencyBodyUnit;
use super::demo_package::{demo, source_body_with_stdlib, source_defaults_with_stdlib, T};
use super::{lowered_body, render, validated};
use crate::ir::{Callee, IrExpr};

/// kotlinc's `fun f(a: Int, b: Int = a, c: Int = 7): Int = b`: each default is lowered in the
/// declaration's own frame, where it may read an earlier parameter, and only a parameter that
/// declares one has a slot filled, exactly as checked FIR lowering lowers the source.
#[test]
fn a_body_publishes_each_serialized_default_in_its_parameter_slot() {
    let fixture = demo(&[("f", &[T::Int, T::Int, T::Int], T::Int)], |_, body| {
        let earlier = body.param(0);
        body.default(1, earlier);
        let seven = body.int(7);
        body.default(2, seven);
        let result = body.param(1);
        Some(vec![body.ret(result)])
    });
    let mut unit = DependencyBodyUnit::default();
    let function = fixture
        .lower(&mut unit, "f", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = "fun f(a: Int, b: Int = a, c: Int = 7): Int {\n return b\n}\n";
    assert_eq!(
        lowered_body(&unit, function),
        source_body_with_stdlib(source, "f", 3)
    );
    let defaults = unit
        .ir()
        .param_defaults(function)
        .expect("the declaration publishes its defaults")
        .iter()
        .map(|default| default.map(|default| render(unit.ir(), default)))
        .collect::<Vec<_>>();
    assert_eq!(defaults, source_defaults_with_stdlib(source, "f", 3));
    assert_eq!(defaults.iter().flatten().count(), 2);
}

/// A declaration without a serialized default publishes no defaults at all, as a source one does.
#[test]
fn a_body_without_defaults_publishes_none() {
    let fixture = demo(&[("f", &[T::Int], T::Int)], |_, body| {
        let result = body.param(0);
        Some(vec![body.ret(result)])
    });
    let mut unit = DependencyBodyUnit::default();
    let function = fixture
        .lower(&mut unit, "f", &[])
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(unit.ir().param_defaults(function), None);
}

/// kotlinc's `fun a(a: Int): Int = b(a)` beside `fun b(a: Int, b: Int = a): Int = b`.
const DEFAULTED_AND_CALLER_SOURCE: &str = "fun b(a: Int, b: Int = a): Int {\n return b\n}\n\
     fun a(a: Int): Int {\n return b(a)\n}\n";

/// [`DEFAULTED_AND_CALLER_SOURCE`] as serialized trees: `b`'s second parameter defaults to its
/// first, and `a` calls `b` leaving it out.
fn defaulted_and_caller() -> super::demo_package::Demo {
    demo(
        &[("b", &[T::Int, T::Int], T::Int), ("a", &[T::Int], T::Int)],
        |name, body| {
            let value = if name == "a" {
                let argument = body.param(0);
                body.call_omitting("b", vec![Some(argument), None], T::Int)
            } else {
                let earlier = body.param(0);
                body.default(1, earlier);
                body.param(1)
            };
            Some(vec![body.ret(value)])
        },
    )
}

/// A body call that leaves `b`'s second parameter to its default calls `b` with that omission,
/// as checked FIR lowering lowers the source call, and the shared default-argument lowering
/// reaches `b`'s entry from it.
#[test]
fn a_body_call_that_leaves_a_parameter_to_its_default_carries_the_omission() {
    let fixture = defaulted_and_caller();
    let mut unit = DependencyBodyUnit::default();
    let caller = fixture
        .lower(&mut unit, "a", &["a", "b"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, caller),
        source_body_with_stdlib(DEFAULTED_AND_CALLER_SOURCE, "a", 1)
    );
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Call { callee, .. } => match &**callee {
                Callee::LocalWithDefaults { function, defaults } => Some((
                    unit.ir().fn_source_names[function].clone(),
                    defaults.to_vec(),
                )),
                other => panic!("the body makes no other call: {other:?}"),
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calls, [("b".to_owned(), vec![1])]);
}

/// The callee of that graph lowers its body and publishes its whole default vector exactly as
/// checked FIR lowering does for the same source.
#[test]
fn the_defaulted_callee_publishes_its_defaults_as_checked_fir_lowering_does() {
    let fixture = defaulted_and_caller();
    let mut unit = DependencyBodyUnit::default();
    let callee = fixture
        .lower(&mut unit, "b", &["b"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, callee),
        source_body_with_stdlib(DEFAULTED_AND_CALLER_SOURCE, "b", 2)
    );
    let defaults = unit
        .ir()
        .param_defaults(callee)
        .expect("the declaration publishes its defaults")
        .iter()
        .map(|default| default.map(|default| render(unit.ir(), default)))
        .collect::<Vec<_>>();
    let expected = source_defaults_with_stdlib(DEFAULTED_AND_CALLER_SOURCE, "b", 2);
    assert!(matches!(expected.as_slice(), [None, Some(_)]));
    assert_eq!(defaults, expected);
}

/// The caller of that graph declares no default and publishes none, as its source does.
#[test]
fn the_caller_without_defaults_publishes_none() {
    let fixture = defaulted_and_caller();
    let mut unit = DependencyBodyUnit::default();
    let caller = fixture
        .lower(&mut unit, "a", &["a", "b"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    assert_eq!(unit.ir().param_defaults(caller), None);
}
