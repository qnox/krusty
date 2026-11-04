//! A dependency function that declares parameter defaults, and a dependency call that leaves a
//! parameter to its default, lowered as checked FIR lowers the equivalent source.

use super::super::*;
use super::demo_package::*;
use super::{lowered_body, render, validated};
use crate::metadata::klib_ir::tree::KlibIrStatement;

const DEFAULTED_AND_CALLER: [Shape; 2] =
    [("b", &[T::Int, T::Int], T::Int), ("a", &[T::Int], T::Int)];

const DEFAULTED_AND_CALLER_SOURCE: &str = "fun b(a: Int, b: Int = a): Int {\n return b\n}\n\
     fun a(a: Int): Int {\n return b(a)\n}\n";

/// `b`'s second parameter defaults to its first; `a` calls `b` leaving it out.
fn defaulted_and_caller(name: &str, body: &mut Body) -> Option<Vec<KlibIrStatement>> {
    let value = match name {
        "a" => {
            let a = body.param(0);
            body.call_omitting("b", vec![Some(a), None], T::Int)
        }
        _ => {
            let first = body.param(0);
            body.default(1, first);
            body.param(1)
        }
    };
    Some(vec![body.ret(value)])
}

#[test]
fn a_call_that_leaves_a_parameter_to_its_default_calls_the_callee_with_its_defaults() {
    let demo = demo(&DEFAULTED_AND_CALLER, defaulted_and_caller);
    let mut unit = DependencyBodyUnit::default();
    let a = demo.lower(&mut unit, "a", &["a", "b"]).expect("lowers");
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, a),
        source_body(DEFAULTED_AND_CALLER_SOURCE, "a", 1)
    );
    let calls = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            crate::ir::IrExpr::Call { callee, .. } => Some(format!("{:?}", **callee)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let b = unit
        .ir()
        .fn_source_names
        .iter()
        .find(|(_, name)| name.as_str() == "b")
        .map(|(function, _)| *function)
        .expect("the callee is declared");
    assert_eq!(
        calls,
        [format!(
            "LocalWithDefaults {{ function: {b}, defaults: [1] }}"
        )]
    );
}

#[test]
fn a_declared_default_is_published_as_checked_fir_lowering_publishes_it() {
    let demo = demo(&DEFAULTED_AND_CALLER, defaulted_and_caller);
    let mut unit = DependencyBodyUnit::default();
    let b = demo.lower(&mut unit, "b", &["b"]).expect("lowers");
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, b),
        source_body(DEFAULTED_AND_CALLER_SOURCE, "b", 2)
    );
    let defaults = unit.ir().fn_params[&b]
        .defaults
        .as_ref()
        .expect("the declaration publishes its defaults");
    let rendered = defaults
        .iter()
        .map(|default| default.map(|default| render(unit.ir(), default)))
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        [
            None,
            Some(source_default(DEFAULTED_AND_CALLER_SOURCE, "b", 2, 1))
        ]
    );
}

#[test]
fn a_function_without_defaults_publishes_none() {
    let demo = demo(&DEFAULTED_AND_CALLER, defaulted_and_caller);
    let mut unit = DependencyBodyUnit::default();
    let a = demo.lower(&mut unit, "a", &["a", "b"]).expect("lowers");
    assert_eq!(unit.ir().fn_params[&a].defaults, None);
}
