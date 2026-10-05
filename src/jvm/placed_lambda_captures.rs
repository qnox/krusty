//! A literal inline lambda that another spliced lambda captures and only invokes.
//!
//! When an inline function's lambda parameter is invoked from inside a lambda the function passes
//! to another inline call (`inline fun foo(x: (Int) -> Unit) { run { x(1) } }`), expanding `foo`
//! leaves the caller's literal for `x` as a capture of `run`'s lambda. kotlinc inlines `run` into
//! `foo` first, so the literal's body replaces each `x.invoke`: it runs in the caller's frame like
//! the body that captures it. The emitter places it at each invocation, and every pass that asks
//! which code runs in a frame must agree with it.

use crate::ir::{for_each_child, ExprId, IrExpr, IrFile};

/// Whether capture `position` of the literal lambda `lambda`, when its body is spliced, is itself a
/// literal inline lambda that body only invokes: its body is then placed at each invocation.
pub(crate) fn is_placed_capture(ir: &IrFile, lambda: ExprId, position: usize) -> bool {
    let IrExpr::Lambda {
        captures,
        inline_body: Some(body),
        ..
    } = ir.expr(lambda)
    else {
        return false;
    };
    let Some(&capture) = captures.get(position) else {
        return false;
    };
    matches!(
        ir.expr(capture),
        IrExpr::Lambda {
            inline_body: Some(_),
            ..
        }
    ) && only_invoked(ir, *body, position as u32)
}

/// Whether every read of value `index` in `body`, a lambda body numbering its own values, invokes
/// it. Nested lambda bodies number their own values; only their captures read this body's.
fn only_invoked(ir: &IrFile, body: ExprId, index: u32) -> bool {
    let mut pending = vec![body];
    while let Some(expression) = pending.pop() {
        match ir.expr(expression) {
            IrExpr::GetValue(value) if *value == index => return false,
            IrExpr::InvokeFunction { func, args, .. } if matches!(ir.expr(*func), IrExpr::GetValue(value) if *value == index) => {
                pending.extend(args.iter().copied())
            }
            IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
            _ => for_each_child(&ir.exprs, expression, &mut |child| pending.push(child)),
        }
    }
    true
}
