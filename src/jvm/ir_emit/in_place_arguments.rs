//! Emission-side admission for reading an `@InlineOnly` call's arguments in place.

use crate::ir::{IrExpr, IrFile, IrTypeOp};
use crate::jvm::classreader::MethodCode;
use crate::jvm::inline::{InPlacePlan, ParameterBinding};
use crate::types::Ty;

pub(super) struct Selection {
    plan: Option<InPlacePlan>,
    top_local: u16,
}

impl Selection {
    pub(super) fn for_call(
        ir: &IrFile,
        call_expression: u32,
        arguments: &[u32],
        inline_only: bool,
        body: &MethodCode,
        descriptor: &str,
        base: u16,
    ) -> Option<Self> {
        let plan = candidate(
            ir,
            call_expression,
            arguments,
            inline_only,
            body,
            descriptor,
        );
        let top_local = match plan.as_ref() {
            Some(plan) => {
                crate::jvm::inline::spliced_frame(body, descriptor, &[], Some(plan), base)?
                    .top_local
            }
            None => base + body.max_locals,
        };
        Some(Self { plan, top_local })
    }

    pub(super) fn binding(&self) -> ParameterBinding<'_> {
        self.plan
            .as_ref()
            .map_or(ParameterBinding::Stored(&[]), ParameterBinding::InPlace)
    }

    pub(super) fn top_local(&self) -> u16 {
        self.top_local
    }
}

fn candidate(
    ir: &IrFile,
    call_expression: u32,
    arguments: &[u32],
    inline_only: bool,
    body: &MethodCode,
    descriptor: &str,
) -> Option<InPlacePlan> {
    if !inline_only || arguments.is_empty() {
        return None;
    }
    // Resolution records the selected declaration's semantic parameters on the call. Consume that
    // normalized type fact: a JVM classifier spelling is representation, not function shape.
    let declared = ir.call_declared_params.get(&call_expression)?;
    if declared.len() != arguments.len()
        || declared.iter().copied().any(is_function_parameter)
        || !arguments
            .iter()
            .all(|&argument| evaluates_without_local_writes(ir, argument))
    {
        return None;
    }
    InPlacePlan::for_body(body, descriptor)
}

fn is_function_parameter(ty: Ty) -> bool {
    matches!(ty.non_null(), Ty::Fun(_))
}

/// Whether an expression's emitted code can neither store a local nor jump. Deliberately narrow:
/// an unlisted shape retains the ordinary stored-parameter path.
fn evaluates_without_local_writes(ir: &IrFile, expression: u32) -> bool {
    match ir.expr(expression) {
        IrExpr::GetValue(_)
        | IrExpr::Const(_)
        | IrExpr::GetStatic(_)
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::EnclosingInstance { .. } => true,
        IrExpr::TypeOp {
            op: IrTypeOp::Cast | IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => evaluates_without_local_writes(ir, *arg),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_parameters_are_recognized_from_semantic_shape_not_jvm_spelling() {
        let function = Ty::fun(vec![Ty::Int], Ty::Int);
        assert!(is_function_parameter(function));
        assert!(is_function_parameter(Ty::nullable(function)));
        assert!(!is_function_parameter(Ty::obj(
            "kotlin/jvm/functions/Function1"
        )));
    }
}
