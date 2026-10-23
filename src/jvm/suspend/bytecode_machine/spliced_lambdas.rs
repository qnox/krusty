//! The bodies a suspend lambda's inline calls place.
//!
//! A suspend lambda every inline call splices is never invoked: each call places its value-form
//! body (`IrExpr::Lambda::inline_body`) where the inline function invokes it. kotlinc writes that
//! body as it writes a suspend function's for inlining: each suspension point between its markers,
//! passing a fake continuation, which the method the body lands in replaces with its own before
//! its coroutine transformer builds the machine over it. A body placed in a suspend function's own frame belongs to
//! that function's machine instead, which collects its points itself.

use crate::fx_hash::HashSet;
use crate::ir::{ExprId, IrExpr, IrFile};

use super::super::append_continuation;
use super::super::cps::TransformedSuspension;
use super::super::lambda_placement::retained_in_an_object;
use super::{owned_suspensions, Route, Subject};

/// The suspension points of a suspend lambda's placed bodies.
#[derive(Default)]
struct PlacedSuspensions {
    suspensions: Vec<TransformedSuspension>,
    /// The continuation each point passes, written as a fake one.
    continuations: Vec<ExprId>,
}

/// Give the placed bodies of each suspend lambda in `fids` whose machine the transformer took, and
/// whose every value an inline call splices, their suspension points, and add them to the lambda's
/// machine. This runs once every function is lowered: lowering a function that holds such a value
/// gives its body nodes of its own, the placed body's among them. `false` when a call could not
/// be given its continuation.
pub(in crate::jvm::suspend) fn place_bodies(
    fids: &[u32],
    ir: &mut IrFile,
    mut route: Route<'_, '_, '_>,
) -> bool {
    for &fid in fids {
        if route.machines.transformed(fid).is_none() {
            continue;
        }
        let Some(values) = crate::jvm::lambda_classes::spliced_values(ir, fid) else {
            continue;
        };
        let Some(placed) = placed_suspensions(ir, fid, &values, &mut route) else {
            return false;
        };
        route
            .machines
            .add_placed_suspensions(fid, placed.suspensions, placed.continuations);
    }
    true
}

/// Give each placed body of `fid`'s lambda values `values` the CPS form of its suspension points,
/// which pass a fake continuation, and return the points. `None` when a call could not be given
/// its continuation.
fn placed_suspensions(
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
        if placed_in_suspend_frame(ir, value, route.suspend_set) {
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

/// Whether the lambda value `value` is passed to a suspend inline call that invokes it in its own
/// body: the caller's lowering takes the body's suspension points where the call places it. A
/// suspend callee whose body hands the lambda to an object it constructs places it in that object's
/// method, as does every function that is not suspend, which cannot invoke a suspend lambda.
fn placed_in_suspend_frame(ir: &IrFile, value: ExprId, suspend_set: &HashSet<u32>) -> bool {
    ir.exprs.iter().enumerate().any(|(call, expression)| {
        let call = ExprId::try_from(call).expect("IR expression index exceeds ExprId");
        let IrExpr::Call { args, .. } = expression else {
            return false;
        };
        args.iter().enumerate().any(|(position, &argument)| {
            argument == value && !retained_in_an_object(ir, call, position)
        }) && super::super::is_suspension_point(ir, call, suspend_set)
    })
}

/// Whether `value` is a suspend lambda whose body an inline call places in an object it constructs,
/// not in the frame of the function the lambda is written in.
pub(super) fn placed_in_an_object(ir: &IrFile, value: ExprId, suspend_set: &HashSet<u32>) -> bool {
    matches!(&ir.exprs[value as usize], IrExpr::Lambda { impl_fn, .. } if suspend_set.contains(impl_fn))
        && !placed_in_suspend_frame(ir, value, suspend_set)
}
