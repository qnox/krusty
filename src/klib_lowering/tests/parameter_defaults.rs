//! Serialized parameter defaults, published as checked FIR lowering publishes a source
//! declaration's.

use super::super::DependencyBodyUnit;
use super::demo_package::{demo, source_body_with_stdlib, source_defaults_with_stdlib, T};
use super::{lowered_body, render, validated};

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
