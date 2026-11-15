//! The bodies of literals passed to `crossinline` parameters.
//!
//! A `crossinline` lambda may be inlined into an object or lambda the callee creates, a class of
//! its own, so kotlinc compiles its body as code outside the class it is written in: a property
//! goes through its accessors and a private declaration through a synthetic accessor
//! (`SyntheticAccessorLowering`), whichever class the body ends up in. Everything written inside
//! such a body, nested lambdas included, is in that position too.

use krusty::fx_hash::HashSet;
use krusty::ir::{ExprId, IrExpr, IrFile};
use krusty::types::InlineParameterModifier;

/// The inline body of each literal an inline call passes to a `crossinline` parameter, whichever
/// module declares the callee: every realized inline call publishes its operands' modifiers, as
/// the JVM inliner treats them ([`crate::suspend_coroutine_blocks`]).
pub(super) fn crossinline_literal_bodies(ir: &IrFile) -> HashSet<ExprId> {
    let mut bodies = HashSet::default();
    for (&call, modifiers) in &ir.call_inline_modifiers {
        let arguments: Vec<Option<ExprId>> = match ir.expr(call) {
            IrExpr::Call { args, .. } => args.iter().copied().map(Some).collect(),
            IrExpr::MethodCall { args, .. } => args.clone(),
            _ => continue,
        };
        for (argument, modifier) in arguments.into_iter().zip(modifiers.iter()) {
            let Some(argument) = argument else { continue };
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
