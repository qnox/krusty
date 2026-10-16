//! Operator-convention selection for compact signature evaluation.

use super::*;

pub(super) fn select_binary(
    semantics: &ProductionSignatureSemantics<'_>,
    scope: crate::fir::SignatureScope,
    operator: crate::fir::SigBinaryOperator,
    origin: crate::fir::OriginId,
    lhs: crate::fir::ResolvedTy,
    rhs: crate::fir::ResolvedTy,
    demand: &mut dyn FnMut(
        crate::fir::DeclarationId,
    ) -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
    let convention = match operator {
        crate::fir::SigBinaryOperator::Add => Some(("plus", false)),
        crate::fir::SigBinaryOperator::Subtract => Some(("minus", false)),
        crate::fir::SigBinaryOperator::Multiply => Some(("times", false)),
        crate::fir::SigBinaryOperator::Divide => Some(("div", false)),
        crate::fir::SigBinaryOperator::Remainder => Some(("rem", false)),
        crate::fir::SigBinaryOperator::Less
        | crate::fir::SigBinaryOperator::LessOrEqual
        | crate::fir::SigBinaryOperator::Greater
        | crate::fir::SigBinaryOperator::GreaterOrEqual => Some(("compareTo", true)),
        crate::fir::SigBinaryOperator::Equal
        | crate::fir::SigBinaryOperator::NotEqual
        | crate::fir::SigBinaryOperator::BooleanAnd
        | crate::fir::SigBinaryOperator::BooleanOr
        | crate::fir::SigBinaryOperator::ReferentialEqual
        | crate::fir::SigBinaryOperator::ReferentialNotEqual => None,
    };
    if let Some((name, comparison)) = convention {
        let selected =
            semantics
                .with_resolver(scope, |resolver| {
                    match super::super::select_delegate_operator(
                    resolver,
                    lhs.get(),
                    name,
                    &[rhs.get()],
                ) {
                    crate::resolve::delegated_properties::DelegateConventionSelection::Selected(
                        selected,
                        result,
                    ) => Some((selected, result)),
                    crate::resolve::delegated_properties::DelegateConventionSelection::None(_)
                    | crate::resolve::delegated_properties::DelegateConventionSelection::Ambiguous(
                        _,
                    ) => None,
                }
                })
                .ok();
        if let Some((selected, result)) = selected {
            let selected_result = semantics.selected_convention_result(
                lhs.get(),
                &selected,
                result,
                &[rhs.get()],
                None,
                demand,
            )?;
            if comparison {
                if selected_result.get() != Ty::Int {
                    return Err(ProductionSignatureSemantics::failure());
                }
                return crate::fir::ResolvedTy::new(Ty::Boolean)
                    .map_err(|_| ProductionSignatureSemantics::failure());
            }
            return Ok(selected_result);
        }
    }
    let operator = match operator {
        crate::fir::SigBinaryOperator::Add => crate::ast::BinOp::Add,
        crate::fir::SigBinaryOperator::Subtract => crate::ast::BinOp::Sub,
        crate::fir::SigBinaryOperator::Multiply => crate::ast::BinOp::Mul,
        crate::fir::SigBinaryOperator::Divide => crate::ast::BinOp::Div,
        crate::fir::SigBinaryOperator::Remainder => crate::ast::BinOp::Rem,
        crate::fir::SigBinaryOperator::Equal => crate::ast::BinOp::Eq,
        crate::fir::SigBinaryOperator::NotEqual => crate::ast::BinOp::Ne,
        crate::fir::SigBinaryOperator::Less => crate::ast::BinOp::Lt,
        crate::fir::SigBinaryOperator::LessOrEqual => crate::ast::BinOp::Le,
        crate::fir::SigBinaryOperator::Greater => crate::ast::BinOp::Gt,
        crate::fir::SigBinaryOperator::GreaterOrEqual => crate::ast::BinOp::Ge,
        crate::fir::SigBinaryOperator::BooleanAnd => crate::ast::BinOp::And,
        crate::fir::SigBinaryOperator::BooleanOr => crate::ast::BinOp::Or,
        crate::fir::SigBinaryOperator::ReferentialEqual => crate::ast::BinOp::RefEq,
        crate::fir::SigBinaryOperator::ReferentialNotEqual => crate::ast::BinOp::RefNe,
    };
    semantics.checked_binary(scope, origin, operator, lhs.get(), rhs.get())
}
