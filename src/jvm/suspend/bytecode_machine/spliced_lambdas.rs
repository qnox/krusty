//! The bodies a suspend lambda's inline calls place.
//!
//! A suspend lambda every inline call splices is never invoked: each call places its value-form
//! body (`IrExpr::Lambda::inline_body`) where the inline function invokes it. kotlinc writes that
//! body as it writes a suspend function's for inlining: each suspension point between its markers,
//! passing a fake continuation, which the method the body lands in replaces with its own before
//! its coroutine transformer builds the machine over it. A body placed in a suspend function's own frame belongs to
//! that function's machine instead, which collects its points itself.

use crate::ir::{ExprId, IrExpr, IrFile};

use super::super::append_continuation;
use super::super::cps::TransformedSuspension;
use super::{owned_suspensions, Route, Subject};

/// The suspension points of a suspend lambda's placed bodies.
#[derive(Default)]
pub(super) struct PlacedSuspensions {
    pub(super) suspensions: Vec<TransformedSuspension>,
    /// The continuation each point passes, written as a fake one.
    pub(super) continuations: Vec<ExprId>,
}

/// Give each placed body of `fid`'s lambda values `values` the CPS form of its suspension points,
/// which pass a fake continuation, and return the points. `None` when a call could not be given
/// its continuation.
pub(super) fn placed_suspensions(
    ir: &mut IrFile,
    fid: u32,
    values: &[ExprId],
    route: &mut Route<'_, '_, '_>,
) -> Option<PlacedSuspensions> {
    let mut placed = PlacedSuspensions::default();
    for &value in values {
        let IrExpr::Lambda {
            inline_body: Some(body),
            ..
        } = *ir.expr(value)
        else {
            continue;
        };
        if placed_in_suspend_frame(ir, value, route) {
            continue;
        }
        let suspensions = owned_suspensions(ir, fid, body, route, Subject::NamedFunction)?;
        for suspension in &suspensions {
            if ir.is_unintercepted_suspension(suspension.call) {
                continue;
            }
            let continuation = ir.add_expr(IrExpr::CurrentContinuation);
            if let Some(line) = ir.dispatch_line(suspension.call) {
                ir.expr_source_lines.insert(continuation, line);
            }
            if !append_continuation(
                ir,
                suspension.call,
                continuation,
                route.outputs.default_call_operands,
            ) {
                return None;
            }
            placed.continuations.push(continuation);
        }
        placed.suspensions.extend(suspensions);
    }
    Some(placed)
}

/// Whether the lambda value `value` is written in a suspend function, whose own lowering takes the
/// body's suspension points where an inline suspend call places it.
fn placed_in_suspend_frame(ir: &IrFile, value: ExprId, route: &Route<'_, '_, '_>) -> bool {
    route.suspend_set.iter().any(|&function| {
        ir.functions[function as usize]
            .body
            .is_some_and(|body| crate::ir::value_namespace_expressions(ir, body).contains(&value))
    })
}
