use crate::ir::{ExprId, IrCheckedArgument, IrDeclarationArgumentBoundary, IrExpr, IrFile};
use crate::types::Ty;

/// Whether source-order operands already follow declaration parameter order.
pub(in super::super) fn follow_parameter_order(
    arguments: &[IrCheckedArgument],
    preceding_parameter: Option<u32>,
) -> bool {
    let mut previous = preceding_parameter;
    for argument in arguments {
        let parameter = match argument {
            IrCheckedArgument::Expression { parameter, .. } => *parameter,
            IrCheckedArgument::Vararg {
                parameter,
                elements,
                ..
            } if !elements.is_empty() => *parameter,
            IrCheckedArgument::Default { .. } | IrCheckedArgument::Vararg { .. } => continue,
        };
        if previous.is_some_and(|previous| parameter < previous) {
            return false;
        }
        previous = Some(parameter);
    }
    true
}

/// Publish supplied call edges whose declaration parameter mentions a type parameter.
///
/// The edge is the checked argument-to-parameter identity. Whether that parameter's class bound
/// later needs a platform carrier is a target decision. Omitted defaults never acquire an edge.
pub(super) fn record(
    ir: &mut IrFile,
    call: ExprId,
    declaration_parameters: &[Ty],
    omitted: &[u32],
) {
    let omitted = omitted
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let edges = match ir.expr(call) {
        IrExpr::Call { args, .. } => (0..declaration_parameters.len() as u32)
            .filter(|parameter| !omitted.contains(parameter))
            .zip(args.iter().copied())
            .collect::<Vec<_>>(),
        IrExpr::MethodCall { args, .. } => args
            .iter()
            .enumerate()
            .filter_map(|(parameter, argument)| Some((parameter as u32, (*argument)?)))
            .collect(),
        _ => return,
    };
    let boundaries = edges
        .into_iter()
        .filter_map(|(parameter, argument)| {
            let declaration = *declaration_parameters.get(parameter as usize)?;
            crate::types::ty_mentions_any_param(declaration).then_some(
                IrDeclarationArgumentBoundary {
                    argument,
                    parameter,
                    declaration,
                    retarget_coercion: match ir.expr(argument) {
                        IrExpr::TypeOp {
                            op: crate::ir::IrTypeOp::ImplicitCoercion,
                            type_operand,
                            ..
                        } => crate::types::ty_mentions_any_param(*type_operand),
                        _ => false,
                    },
                },
            )
        })
        .collect::<Vec<_>>();
    if !boundaries.is_empty() {
        ir.declaration_argument_boundaries
            .insert(call, boundaries.into_boxed_slice());
    }
}
