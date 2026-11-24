//! Value-class representation at checked call-argument boundaries.
//!
//! The selected declaration's semantic parameter vector is authoritative for every call shape. Only
//! a backend-built operation (an intrinsic or a representation adapter such as `box-impl`) has no
//! declaration and takes its operands as they are. Any other call that reaches this pass without a
//! vector is a lowering defect, reported here rather than adapted from a JVM descriptor.

use super::{record_value_boundary, BoxOp, ReprCtx, Under};
use krusty::ir::{Callee, ExprId, FunId, IrExpr, IrFile};
use krusty::types::Ty;

pub(super) fn record_boundaries(
    callee: &Callee,
    declared_parameters: Option<&[Ty]>,
    arguments: &[ExprId],
    under: &Under,
    context: &ReprCtx<'_>,
    operations: &mut Vec<(ExprId, BoxOp)>,
) {
    let Some(parameters) = declared_parameters else {
        assert!(
            arguments.is_empty()
                || matches!(
                    callee,
                    Callee::Intrinsic { .. } | Callee::ValueClassAdapter { .. }
                ),
            "a call reached value-class lowering without its declared parameters: {callee:?}"
        );
        return;
    };
    for (&argument, &parameter) in arguments.iter().zip(parameters) {
        let (value, _) = context.through_erased_generic_coercion(argument);
        // A type parameter bounded by a value class is passed as that class's own occurrence.
        let parameter = super::member_names::value_class_bound_occurrence(parameter, under);
        record_value_boundary(operations, context.exprs, context, value, parameter, under);
    }
}

/// Record the declared parameters of each member call into `functions`, overrides whose value-class
/// parameter holds its box in the overridden declaration's generic slot. The emitter then passes
/// the box a caller makes there as it is, rather than unboxing it to the carrier the slot shares.
pub(super) fn record_method_parameters(
    ir: &mut IrFile,
    functions: &krusty::fx_hash::HashSet<FunId>,
    parameters: &[Vec<Ty>],
) {
    for (id, expression) in ir.exprs.iter().enumerate() {
        let IrExpr::MethodCall { class, index, .. } = expression else {
            continue;
        };
        let function = ir.classes[*class as usize].methods[*index as usize];
        if functions.contains(&function) {
            let declared = parameters[function as usize].clone().into_boxed_slice();
            ir.call_declared_params.insert(id as ExprId, declared);
        }
    }
}
