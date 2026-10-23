//! Serialized return targets remain distinct from the template function's own return target.

use super::demo_package::{demo, Demo, T};
use super::{local_symbol, validated, DependencyBodyUnit};
use crate::ir::{IrConst, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrExprKind, KlibIrStatement};
use crate::metadata::klib_ir::KlibIrSymbolKind;

fn conditional_returns(has_else: bool, target_kind: KlibIrSymbolKind) -> Demo {
    demo(&[("f", &[T::Boolean], T::Int)], move |_, body| {
        let target = local_symbol(target_kind, 500);
        let condition = body.param(0);
        let one = body.int(1);
        let left = body.expr(
            T::Nothing,
            KlibIrExprKind::Return {
                target: target.clone(),
                value: one,
            },
        );
        let mut branches = vec![(condition, left)];
        if has_else {
            let otherwise = body.otherwise();
            let two = body.int(2);
            let right = body.expr(
                T::Nothing,
                KlibIrExprKind::Return {
                    target: target.clone(),
                    value: two,
                },
            );
            branches.push((otherwise, right));
        }
        let conditional =
            body.branches("IF", branches, if has_else { T::Nothing } else { T::Unit });
        let frame = body.expr(
            T::Int,
            KlibIrExprKind::ReturnableBlock {
                symbol: target,
                statements: vec![KlibIrStatement::Expression(conditional)],
                origin: None,
            },
        );
        Some(vec![body.ret(frame)])
    })
}

#[test]
fn conditional_block_returns_store_their_values_and_leave_only_the_exact_frame() {
    let demo = conditional_returns(true, KlibIrSymbolKind::ReturnableBlock);
    let mut unit = DependencyBodyUnit::default();
    demo.lower(&mut unit, "f", &[])
        .expect("both branches return their frame's result");
    let ir = unit.ir();
    validated(ir);
    let frames = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::While { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [Some(label)] = frames.as_slice() else {
        panic!("expected one labelled frame, got {frames:?}");
    };
    let exits = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Break { label } => Some(label.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(exits, vec![Some(label.clone()); 3]);
    let stores = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::SetValue { var, value } => {
                let IrExpr::Const(value) = ir.expr(*value) else {
                    panic!("expected a returned constant, got {:?}", ir.expr(*value));
                };
                Some((*var, value.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stores, [(1, IrConst::Int(1)), (1, IrConst::Int(2)),]);
    let function_returns = ir
        .exprs
        .iter()
        .filter(|expression| matches!(expression, IrExpr::Return(_)))
        .count();
    assert_eq!(function_returns, 1);
    assert_eq!(
        ir.checked_return_depths
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [0]
    );
}

#[test]
fn a_value_frame_that_may_fall_through_never_reads_a_zero_initialized_result() {
    assert_eq!(
        conditional_returns(false, KlibIrSymbolKind::ReturnableBlock).declined("f", &[]),
        "the KLIB body of `demo.f` (it uses a value-returning block that may complete without a value)"
    );
}

#[test]
fn a_block_target_must_have_a_returnable_block_identity() {
    assert_eq!(
        conditional_returns(true, KlibIrSymbolKind::Function).declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: a returnable block has no returnable-block symbol)"
    );
}

#[test]
fn a_return_target_is_not_a_declared_value_that_can_read_the_result_temporary() {
    let fixture = demo(&[("f", &[T::Boolean], T::Int)], |_, body| {
        let target = local_symbol(KlibIrSymbolKind::ReturnableBlock, 500);
        let condition = body.param(0);
        let read = body.expr(
            T::Int,
            KlibIrExprKind::GetValue {
                symbol: target.clone(),
                origin: None,
            },
        );
        let early = body.expr(
            T::Nothing,
            KlibIrExprKind::Return {
                target: target.clone(),
                value: read,
            },
        );
        let conditional = body.branches("IF", vec![(condition, early)], T::Unit);
        let value = body.int(2);
        let tail = body.expr(
            T::Nothing,
            KlibIrExprKind::Return {
                target: target.clone(),
                value,
            },
        );
        let frame = body.expr(
            T::Int,
            KlibIrExprKind::ReturnableBlock {
                symbol: target,
                statements: vec![
                    KlibIrStatement::Expression(conditional),
                    KlibIrStatement::Expression(tail),
                ],
                origin: None,
            },
        );
        Some(vec![body.ret(frame)])
    });
    assert_eq!(
        fixture.declined("f", &[]),
        "the KLIB body of `demo.f` (it reads a value it does not declare)"
    );
}

#[test]
fn a_nested_tail_block_cannot_redeclare_an_enclosing_frame_target() {
    let fixture = demo(&[("f", &[T::Boolean], T::Int)], |_, body| {
        let target = local_symbol(KlibIrSymbolKind::ReturnableBlock, 500);
        let condition = body.param(0);
        let one = body.int(1);
        let inner_return = body.expr(
            T::Nothing,
            KlibIrExprKind::Return {
                target: target.clone(),
                value: one,
            },
        );
        let inner = body.expr(
            T::Int,
            KlibIrExprKind::ReturnableBlock {
                symbol: target.clone(),
                statements: vec![KlibIrStatement::Expression(inner_return)],
                origin: None,
            },
        );
        let conditional = body.branches("IF", vec![(condition, inner)], T::Unit);
        let two = body.int(2);
        let tail = body.expr(
            T::Nothing,
            KlibIrExprKind::Return {
                target: target.clone(),
                value: two,
            },
        );
        let frame = body.expr(
            T::Int,
            KlibIrExprKind::ReturnableBlock {
                symbol: target,
                statements: vec![
                    KlibIrStatement::Expression(conditional),
                    KlibIrStatement::Expression(tail),
                ],
                origin: None,
            },
        );
        Some(vec![body.ret(frame)])
    });
    assert_eq!(
        fixture.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: a returnable block repeats an enclosing target's identity)"
    );
}

#[test]
fn two_tail_only_blocks_cannot_declare_the_same_return_target() {
    let fixture = demo(&[("f", &[], T::Unit)], |_, body| {
        let target = local_symbol(KlibIrSymbolKind::ReturnableBlock, 500);
        let inner = body.expr(
            T::Unit,
            KlibIrExprKind::ReturnableBlock {
                symbol: target.clone(),
                statements: Vec::new(),
                origin: None,
            },
        );
        let tail = body.expr(
            T::Nothing,
            KlibIrExprKind::Return {
                target: target.clone(),
                value: inner,
            },
        );
        let outer = body.expr(
            T::Unit,
            KlibIrExprKind::ReturnableBlock {
                symbol: target,
                statements: vec![KlibIrStatement::Expression(tail)],
                origin: None,
            },
        );
        Some(vec![body.ret(outer)])
    });
    assert_eq!(
        fixture.declined("f", &[]),
        "the KLIB body of `demo.f` (its serialized declaration disagrees with the selected one: a returnable block repeats an enclosing target's identity)"
    );
}
