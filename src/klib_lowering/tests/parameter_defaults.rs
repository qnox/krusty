//! Serialized parameter defaults, published as checked FIR lowering publishes a source
//! declaration's.

use super::super::DependencyBodyUnit;
use super::demo_package::{demo, source_body_with_stdlib, source_defaults_with_stdlib, T};
use super::{lowered_body, public_symbol, render, validated};
use crate::metadata::klib_ir::tree::{KlibIrArguments, KlibIrExprKind, KlibIrMemberAccess};
use crate::metadata::klib_ir::KlibIrSymbolKind;

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

/// kotlinc's `fun g(a: Int, b: Int = a): Int = b` and `fun f(x: Int): Int = g(x)`: the serialized
/// call leaves `b` out, and lowers as a source call of a function of its own file leaving it to
/// its default does, carrying the supplied argument and the omitted parameter.
fn omitting_fixture(g_declares_the_default: bool) -> super::demo_package::Demo {
    demo(
        &[("g", &[T::Int, T::Int], T::Int), ("f", &[T::Int], T::Int)],
        move |name, body| match name {
            "g" => {
                if g_declares_the_default {
                    let earlier = body.param(0);
                    body.default(1, earlier);
                }
                let result = body.param(1);
                Some(vec![body.ret(result)])
            }
            "f" => {
                let x = body.param(0);
                let signature = body.signatures["g"].clone();
                let call = body.expr(
                    T::Int,
                    KlibIrExprKind::Call {
                        access: KlibIrMemberAccess {
                            symbol: public_symbol(KlibIrSymbolKind::Function, signature),
                            arguments: KlibIrArguments::Flat(vec![Some(x), None]),
                            type_arguments: Vec::new(),
                            origin: None,
                        },
                        super_qualifier: None,
                    },
                );
                Some(vec![body.ret(call)])
            }
            _ => None,
        },
    )
}

#[test]
fn a_call_leaving_a_parameter_to_its_default_carries_the_omission() {
    let fixture = omitting_fixture(true);
    let mut unit = DependencyBodyUnit::default();
    let function = fixture
        .lower(&mut unit, "f", &["g"])
        .unwrap_or_else(|decline| panic!("{decline}"));
    validated(unit.ir());
    let source = "fun g(a: Int, b: Int = a): Int {\n return b\n}\n\
                  fun f(x: Int): Int {\n return g(x)\n}\n";
    assert_eq!(
        lowered_body(&unit, function),
        source_body_with_stdlib(source, "f", 1)
    );
    let ir = unit.ir();
    let calls = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            crate::ir::IrExpr::Call { callee, args, .. } => match &**callee {
                crate::ir::Callee::LocalWithDefaults { function, defaults } => {
                    Some((*function, defaults.to_vec(), args.len()))
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(g, omitted, supplied)] = calls.as_slice() else {
        panic!("one call leaves an argument out: {calls:?}");
    };
    assert_eq!((omitted.as_slice(), *supplied), ([1].as_slice(), 1));
    assert_eq!(
        ir.param_defaults(*g)
            .expect("g publishes its default")
            .iter()
            .map(Option::is_some)
            .collect::<Vec<_>>(),
        [false, true]
    );
}

/// An argument left out of a call of a parameter without a default has nothing to fill it: the
/// body declines rather than inventing a value.
#[test]
fn a_call_leaving_out_an_argument_without_a_default_declines() {
    let fixture = omitting_fixture(false);
    let mut unit = DependencyBodyUnit::default();
    let decline = fixture
        .lower(&mut unit, "f", &["g"])
        .expect_err("g declares no default for `b`");
    assert_eq!(
        decline.to_string(),
        "the KLIB body of `demo.f` (it uses a call that leaves a parameter to its default)"
    );
    assert!(unit.ir().functions.is_empty());
}
