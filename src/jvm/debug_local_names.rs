//! JVM spelling of backend-neutral common-IR debug-local provenance.

use crate::ir::{ExprId, IrDebugLocalProvenance, IrFile, IrInlineLocalRole, IrLambdaOrigin};

pub(super) fn lambda_implementation_name(origin: &IrLambdaOrigin) -> String {
    let enclosing = if origin.implementation_name.is_empty() {
        "_init_"
    } else {
        origin.implementation_name.as_str()
    };
    format!("{enclosing}$lambda${}", origin.implementation_ordinal)
}

/// Render one source/debug local at the JVM boundary. Common lowering records source spelling,
/// inline depth, and stable lambda identity; only this module owns kotlinc's `$this$`, `$iv`, and
/// `_u24` conventions.
pub(super) fn name(ir: &IrFile, declaration: ExprId) -> Option<String> {
    match ir.debug_local_provenance(declaration) {
        Some(IrDebugLocalProvenance::InlineValue { role, depth }) => {
            let source = ir.value_names.get(&declaration)?;
            let escaped = source.replace('$', "_u24");
            let mut rendered = match role {
                IrInlineLocalRole::Value => escaped,
                IrInlineLocalRole::DispatchReceiver => format!("$this${escaped}"),
            };
            for _ in 0..depth {
                rendered.push_str("$iv");
            }
            Some(rendered)
        }
        Some(IrDebugLocalProvenance::InlineLambdaReceiver { implementation }) => {
            let origin = ir.lambda_origins.get(&implementation)?;
            Some(format!(
                "$this${}",
                lambda_implementation_name(origin).replace('$', "_u24")
            ))
        }
        None => ir.value_names.get(&declaration).cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrExpr, IrFunction};
    use crate::types::Ty;

    fn local(ir: &mut IrFile, source_name: &str) -> ExprId {
        let index = declaration_index(ir);
        let declaration = ir.add_expr(IrExpr::Variable {
            index,
            ty: Ty::String,
            init: None,
            named: true,
        });
        ir.value_names.insert(declaration, source_name.to_string());
        declaration
    }

    fn declaration_index(ir: &IrFile) -> u32 {
        u32::try_from(ir.exprs.len()).expect("test expression index")
    }

    #[test]
    fn inline_value_escaping_and_depth_are_jvm_owned() {
        let mut ir = IrFile::default();
        let declaration = local(&mut ir, "cost$raw");
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::inline_value(IrInlineLocalRole::Value, 2),
        );
        assert_eq!(name(&ir, declaration).as_deref(), Some("cost_u24raw$iv$iv"));
    }

    #[test]
    fn inline_dispatch_receiver_uses_the_jvm_receiver_convention() {
        let mut ir = IrFile::default();
        let declaration = local(&mut ir, "map");
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::inline_value(IrInlineLocalRole::DispatchReceiver, 1),
        );
        assert_eq!(name(&ir, declaration).as_deref(), Some("$this$map$iv"));
    }

    #[test]
    fn inline_lambda_receiver_uses_the_stable_implementation_origin() {
        let mut ir = IrFile::default();
        let implementation = ir.add_fun(IrFunction {
            name: "$fir_lambda".into(),
            param_checks: Vec::new(),
            params: vec![Ty::String],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
        });
        ir.lambda_origins.insert(
            implementation,
            IrLambdaOrigin {
                identity: 0,
                lexical_owner: None,
                enclosing_name: "nested".into(),
                binding_name: None,
                ordinal: 0,
                implementation_name: "nested".into(),
                implementation_ordinal: 0,
                receiver_parameter: Some(0),
            },
        );
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: Ty::String,
            init: None,
            named: true,
        });
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::InlineLambdaReceiver { implementation },
        );
        assert_eq!(
            name(&ir, declaration).as_deref(),
            Some("$this$nested_u24lambda_u240")
        );
    }
}
