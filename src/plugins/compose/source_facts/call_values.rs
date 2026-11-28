//! The values a composable call's arguments read, and kotlinc's `argumentMetaOf` verdict on each:
//! a function's parameters, or the captures of a lambda, classified against the caller's frame.
use crate::ir::{ExprId, FunId, IrExpr, IrFile};

use super::ArgumentState;

/// The values a body's composable calls read: a function's parameters, or a lambda's captures.
#[derive(Clone, Copy)]
pub(super) enum CallValues<'a> {
    /// A function with this many value parameters.
    Parameters(u32),
    /// A lambda's captures, each flagged when it captures a local variable, not a parameter, and
    /// the number of values it is itself called with, which follow them.
    Captures(&'a [CaptureKind], u32),
}

/// What a lambda's capture is to the arguments that read it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureKind {
    /// A parameter of the function that writes the lambda, which the callee compares itself.
    Parameter,
    /// A local whose declaration's state a lambda's body cannot see.
    Opaque,
    /// A local `val` initialized with a constant.
    Static,
}

impl CallValues<'_> {
    fn count(self) -> u32 {
        match self {
            CallValues::Parameters(count) => count,
            CallValues::Captures(locals, parameters) => locals.len() as u32 + parameters,
        }
    }

    /// The ordinal of the parameter value `index` is, whose state the body's `$dirty` bits hold: a
    /// function's parameter, or a value its lambda is called with, which follows the captures.
    fn tracked(self, index: u32) -> Option<u32> {
        match self {
            CallValues::Parameters(count) => (index < count).then_some(index),
            CallValues::Captures(locals, parameters) => {
                let captures = locals.len() as u32;
                (captures..captures + parameters)
                    .contains(&index)
                    .then(|| index - captures)
            }
        }
    }

    /// Whether `expression` reads a captured local, whose static-ness kotlinc takes from the
    /// local's declaration (a `val` with a static initializer is static).
    pub(super) fn reads_captured_local(self, ir: &IrFile, expression: ExprId) -> bool {
        let CallValues::Captures(locals, _) = self else {
            return false;
        };
        let mut pending = vec![expression];
        while let Some(expression) = pending.pop() {
            if let IrExpr::GetValue(index) = ir.exprs[expression as usize] {
                if locals.get(index as usize) == Some(&CaptureKind::Opaque) {
                    return true;
                }
            }
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        }
        false
    }
}

/// What the value `index` of `caller`'s frame is to a lambda that captures it: a parameter, or a
/// local declared once, whose initializer a constant makes static.
pub(super) fn capture_kind(ir: &IrFile, caller: FunId, index: u32) -> CaptureKind {
    let function = &ir.functions[caller as usize];
    let frame = function.params.len() + usize::from(function.dispatch_receiver.is_some());
    if (index as usize) < frame {
        return CaptureKind::Parameter;
    }
    let mut initializers = Vec::new();
    let mut reassigned = false;
    let mut pending = function.body.into_iter().collect::<Vec<_>>();
    while let Some(expression) = pending.pop() {
        match ir.exprs[expression as usize] {
            IrExpr::Variable {
                index: declared,
                init: Some(init),
                ..
            } if declared == index => initializers.push(init),
            IrExpr::SetValue { var, .. } if var == index => reassigned = true,
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    match initializers.as_slice() {
        [_] if reassigned => CaptureKind::Opaque,
        [initializer] if matches!(ir.exprs[*initializer as usize], IrExpr::Const(_)) => {
            CaptureKind::Static
        }
        [initializer] if reads_any_parameter(ir, *initializer, frame as u32) => {
            CaptureKind::Parameter
        }
        _ => CaptureKind::Opaque,
    }
}

/// Whether any expression under `expression` reads one of the first `parameters` values.
fn reads_any_parameter(ir: &IrFile, expression: ExprId, parameters: u32) -> bool {
    let mut pending = vec![expression];
    while let Some(expression) = pending.pop() {
        if matches!(ir.exprs[expression as usize], IrExpr::GetValue(index) if index < parameters) {
            return true;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    false
}

/// kotlinc's `argumentMetaOf` for an argument of a call in a function with `parameters` value
/// parameters, or `None` when its answer depends on more than krusty classifies yet.
pub(super) fn argument_state(
    ir: &IrFile,
    argument: ExprId,
    values: CallValues<'_>,
) -> Option<ArgumentState> {
    match ir.exprs[argument as usize] {
        IrExpr::Const(_) => Some(ArgumentState::Static),
        // A captured value is not a parameter: its callee compares it itself.
        IrExpr::GetValue(index)
            if matches!(values, CallValues::Captures(kinds, _)
                if kinds.get(index as usize) == Some(&CaptureKind::Static)) =>
        {
            Some(ArgumentState::Static)
        }
        IrExpr::GetValue(index) if index < values.count() => Some(
            values
                .tracked(index)
                .map_or(ArgumentState::Uncertain, ArgumentState::Forwarded),
        ),
        _ => reads_parameter(ir, argument, values.count()).then_some(ArgumentState::Uncertain),
    }
}

/// Whether `expression` is an operation over a value parameter. `isStatic` asks every operand of
/// a call, an operator or a conversion to be static, and a parameter never is, so such an
/// expression is never static whatever the operation.
fn reads_parameter(ir: &IrFile, expression: ExprId, parameters: u32) -> bool {
    match &ir.exprs[expression as usize] {
        IrExpr::GetValue(index) => *index < parameters,
        IrExpr::Call { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::PrimitiveBinOp { .. }
        | IrExpr::PrimitiveNeg { .. }
        | IrExpr::Equality { .. }
        | IrExpr::StringConcat(_)
        | IrExpr::TypeOp { .. } => {
            let mut reads = false;
            crate::ir::for_each_child(&ir.exprs, expression, &mut |child| {
                reads |= reads_parameter(ir, child, parameters);
            });
            reads
        }
        _ => false,
    }
}
