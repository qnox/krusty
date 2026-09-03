//! Tail-position rewriting for checked self calls.
//!
//! Calls reach this module only after FIR has selected a stable callable identity and mapped every
//! argument to its declaration parameter. The transform therefore contains no name lookup or
//! overload logic.

use crate::fir::{OriginId, SyntheticOriginKind};
use crate::ir::{Callee, ExprId, FunId, IrConst, IrExpr, IrFile, IrNodeOrigin};

use super::FirLoweringFailure;

const LOOP_LABEL: &str = "$tailrec";

pub(super) fn finish_tailrec_body(
    ir: &mut IrFile,
    mut roots: Vec<ExprId>,
    function: FunId,
    parameter_count: usize,
    origin: OriginId,
) -> Result<ExprId, FirLoweringFailure> {
    let tail = roots
        .pop()
        .ok_or(FirLoweringFailure::MissingBodyResult { origin })?;
    let tail = tail_value(ir, tail, function, parameter_count, origin)?;
    roots.push(tail);
    let loop_body = generated(
        ir,
        IrExpr::Block {
            stmts: roots,
            value: None,
        },
        origin,
    );
    let condition = generated(ir, IrExpr::Const(IrConst::Boolean(true)), origin);
    let loop_expression = generated(
        ir,
        IrExpr::While {
            cond: condition,
            body: loop_body,
            update: None,
            post_test: false,
            label: Some(LOOP_LABEL.to_owned()),
        },
        origin,
    );
    Ok(generated(
        ir,
        IrExpr::Block {
            stmts: vec![loop_expression],
            value: None,
        },
        origin,
    ))
}

fn tail_value(
    ir: &mut IrFile,
    expression: ExprId,
    function: FunId,
    parameter_count: usize,
    origin: OriginId,
) -> Result<ExprId, FirLoweringFailure> {
    match ir.expr(expression).clone() {
        IrExpr::Call {
            callee: Callee::Local(target),
            dispatch_receiver: None,
            args,
        } if target == function && args.len() == parameter_count => {
            let mut updates = Vec::with_capacity(parameter_count + 1);
            for (parameter, value) in args.into_iter().enumerate() {
                updates.push(generated(
                    ir,
                    IrExpr::SetValue {
                        var: u32::try_from(parameter)
                            .expect("tailrec parameter count exceeds packed value ids"),
                        value,
                    },
                    origin,
                ));
            }
            updates.push(generated(
                ir,
                IrExpr::Continue {
                    label: Some(LOOP_LABEL.to_owned()),
                },
                origin,
            ));
            Ok(generated(
                ir,
                IrExpr::Block {
                    stmts: updates,
                    value: None,
                },
                origin,
            ))
        }
        IrExpr::Block { mut stmts, value } => {
            let value = value.ok_or(FirLoweringFailure::MissingBodyResult { origin })?;
            stmts.push(tail_value(ir, value, function, parameter_count, origin)?);
            Ok(generated(ir, IrExpr::Block { stmts, value: None }, origin))
        }
        IrExpr::When { branches } => {
            let branches = branches
                .into_iter()
                .map(|(condition, branch)| {
                    Ok((
                        condition,
                        tail_value(ir, branch, function, parameter_count, origin)?,
                    ))
                })
                .collect::<Result<Vec<_>, FirLoweringFailure>>()?;
            Ok(generated(ir, IrExpr::When { branches }, origin))
        }
        _ => Ok(generated(ir, IrExpr::Return(Some(expression)), origin)),
    }
}

fn generated(ir: &mut IrFile, expression: IrExpr, cause: OriginId) -> ExprId {
    let id = ir.add_expr(expression);
    ir.fir_origins.insert(
        id,
        IrNodeOrigin::Synthetic {
            cause,
            kind: SyntheticOriginKind::GeneratedControlFlow,
        },
    );
    id
}
