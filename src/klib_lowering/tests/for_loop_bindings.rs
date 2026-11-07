//! The decoded iterator-loop binding is evaluated once before the source bindings that read it.

use super::demo_package::{demo, Demo, Shape, T};
use super::{validated, DependencyBodyUnit};
use crate::ir::{IrBindingStability, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrExprKind, KlibIrLoop, KlibIrStatement};

fn iterator_loop(
    origin: &str,
    flags: u64,
    next_origin: Option<&str>,
    foreign: bool,
    premature_read: bool,
) -> Demo {
    let shapes: [Shape; 4] = [
        ("advance", &[T::Int], T::Int),
        ("consume", &[T::Int, T::Int], T::Unit),
        ("condition", &[T::Int], T::Boolean),
        ("f", &[T::Int], T::Unit),
    ];
    demo(&shapes, |name, body| match name {
        "advance" => {
            let value = body.param(0);
            Some(vec![body.ret(value)])
        }
        "consume" => Some(Vec::new()),
        "condition" => {
            let value = body.boolean(false);
            Some(vec![body.ret(value)])
        }
        _ => {
            let input = body.param(0);
            let (iterator, iterator_symbol) =
                body.variable_with(T::Int, Some(input), 0, "FOR_LOOP_ITERATOR");
            let receiver = if foreign {
                body.param(0)
            } else {
                body.get(&iterator_symbol, T::Int)
            };
            let next = body.call_signature(
                body.signatures["advance"].clone(),
                vec![receiver],
                next_origin,
                T::Int,
            );
            let (binding, symbol) = body.variable_with(T::Int, Some(next), flags, origin);
            let value = body.get(&symbol, T::Int);
            let (first, first_symbol) = body.val(T::Int, value);
            let value = body.get(&symbol, T::Int);
            let (second, second_symbol) = body.val(T::Int, value);
            let first_value = body.get(&first_symbol, T::Int);
            let second_value = body.get(&second_symbol, T::Int);
            let consume = body.call("consume", vec![first_value, second_value], T::Unit);
            let source = body.block(vec![KlibIrStatement::Expression(consume)], T::Unit);
            let loop_body = body.expr(
                T::Unit,
                KlibIrExprKind::Block {
                    statements: vec![binding, first, second, KlibIrStatement::Expression(source)],
                    origin: Some("FOR_LOOP_INNER_WHILE".to_owned()),
                },
            );
            let condition = if premature_read {
                let value = body.get(&symbol, T::Int);
                body.call("condition", vec![value], T::Boolean)
            } else {
                body.boolean(false)
            };
            let inner = body.expr(
                T::Unit,
                KlibIrExprKind::While(KlibIrLoop {
                    id: 7,
                    condition,
                    body: Some(loop_body),
                    label: None,
                    origin: Some("FOR_LOOP_INNER_WHILE".to_owned()),
                }),
            );
            let loop_ = body.expr(
                T::Unit,
                KlibIrExprKind::Block {
                    statements: vec![iterator, KlibIrStatement::Expression(inner)],
                    origin: Some("FOR_LOOP".to_owned()),
                },
            );
            Some(vec![KlibIrStatement::Expression(loop_)])
        }
    })
}

#[test]
fn an_iterator_element_is_evaluated_once_before_its_destructured_bindings() {
    for (origin, named) in [
        ("IR_TEMPORARY_VARIABLE", false),
        ("FOR_LOOP_VARIABLE", true),
    ] {
        let fixture = iterator_loop(origin, 0, Some("FOR_LOOP_NEXT"), false, false);
        let mut unit = DependencyBodyUnit::default();
        fixture
            .lower(&mut unit, "f", &["advance", "consume", "condition"])
            .unwrap_or_else(|decline| panic!("{decline}"));
        validated(unit.ir());
        let bodies = unit
            .ir()
            .exprs
            .iter()
            .filter_map(|expression| match expression {
                IrExpr::While { body, .. } => Some(*body),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(bodies.len(), 1);
        let IrExpr::Block { stmts, value } = unit.ir().expr(bodies[0]) else {
            panic!("loop body must remain one block")
        };
        assert_eq!(stmts.len(), 3);
        assert!(
            value.is_some(),
            "the complete source body remains the trailing value"
        );
        let variables = stmts
            .iter()
            .map(|id| match unit.ir().expr(*id) {
                IrExpr::Variable {
                    index,
                    init: Some(init),
                    named,
                    ..
                } => (*index, *init, *named),
                expression => panic!("expected binding, got {expression:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            variables
                .iter()
                .map(|(i, _, n)| (*i, *n))
                .collect::<Vec<_>>(),
            [(2, named), (3, true), (4, true)]
        );
        assert!(matches!(
            unit.ir().expr(variables[0].1),
            IrExpr::Call { .. }
        ));
        assert_eq!(
            unit.ir()
                .exprs
                .iter()
                .filter(|expr| matches!(expr, IrExpr::Call { .. }))
                .count(),
            2
        );
        let IrExpr::Block {
            stmts,
            value: Some(call),
        } = unit.ir().expr(value.expect("source block"))
        else {
            panic!("source body retains its call")
        };
        assert!(stmts.is_empty());
        let IrExpr::Call { args, .. } = unit.ir().expr(*call) else {
            panic!("source call")
        };
        assert_eq!(args.len(), 2);
        assert!(matches!(unit.ir().expr(args[0]), IrExpr::GetValue(3)));
        assert!(matches!(unit.ir().expr(args[1]), IrExpr::GetValue(4)));
        for (_, initializer, _) in &variables[1..] {
            assert!(matches!(unit.ir().expr(*initializer), IrExpr::GetValue(2)));
            assert_eq!(
                unit.ir().binding_read_stability.get(initializer),
                Some(&IrBindingStability::Stable)
            );
        }
    }
}

#[test]
fn a_loop_temporary_requires_the_iterator_next_identity_and_immutable_storage() {
    let unsupported = "the KLIB body of `demo.f` (it uses a `for` loop body that does not open with its variable)";
    for (marker, foreign) in [(None, false), (Some("FOR_LOOP_NEXT"), true)] {
        assert_eq!(
            iterator_loop("IR_TEMPORARY_VARIABLE", 0, marker, foreign, false)
                .declined("f", &["advance", "consume", "condition"]),
            unsupported
        );
    }
    assert_eq!(
        iterator_loop(
            "IR_TEMPORARY_VARIABLE",
            2,
            Some("FOR_LOOP_NEXT"),
            false,
            false
        )
        .declined("f", &["advance", "consume", "condition"]),
        "the KLIB body of `demo.f` (it uses a mutable or annotated compiler temporary)"
    );
}

#[test]
fn a_loop_condition_cannot_read_its_body_binding_and_decline_rolls_back_the_unit() {
    assert_eq!(
        iterator_loop(
            "IR_TEMPORARY_VARIABLE",
            0,
            Some("FOR_LOOP_NEXT"),
            false,
            true
        )
        .declined("f", &["advance", "consume", "condition"]),
        "the KLIB body of `demo.f` (it reads a value it does not declare)"
    );
}
