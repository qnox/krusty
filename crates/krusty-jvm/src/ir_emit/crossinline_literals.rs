//! The bodies of literals passed to `crossinline` parameters.
//!
//! A `crossinline` lambda may be inlined into an object or lambda the callee creates, a class of
//! its own, so kotlinc compiles its body as code outside the class it is written in: a property
//! goes through its accessors and a private declaration through a synthetic accessor
//! (`SyntheticAccessorLowering`), whichever class the body ends up in. Everything written inside
//! such a body, nested lambdas included, is in that position too.

use crate::fx_hash::HashSet;
use crate::ir::{ExprId, IrExpr, IrFile};
use crate::types::InlineParameterModifier;

/// The inline body of each literal an inlined call passes to a `crossinline` parameter.
pub(super) fn crossinline_literal_bodies(ir: &IrFile) -> HashSet<ExprId> {
    let mut bodies = HashSet::default();
    for index in 0..ir.exprs.len() {
        let call = u32::try_from(index).expect("IR expression index exceeds ExprId");
        let Some(arguments) = super::must_inline_lambdas::inlined_call_arguments(ir, call) else {
            continue;
        };
        let Some(modifiers) = ir.call_inline_modifiers.get(&call) else {
            continue;
        };
        for (argument, modifier) in arguments.into_iter().zip(modifiers) {
            if *modifier != InlineParameterModifier::Crossinline {
                continue;
            }
            let body = match ir.expr(argument) {
                IrExpr::Lambda {
                    inline_body: Some(body),
                    ..
                } => Some(*body),
                _ => ir
                    .callable_reference_inline_template(argument)
                    .map(|(_, _, body, _)| body),
            };
            bodies.extend(body);
        }
    }
    bodies
}
