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

/// The name a dependency's own debug local carries once its body is spliced into a caller.
///
/// kotlinc suffixes an inlined value with `$iv` and leaves its inline-depth markers (`$i$f$…`,
/// `$i$a$…`) alone — those already name the inlining they belong to. The suffix is a property of
/// being inlined rather than of the declaration, which is why it is applied here and not stored.
pub(super) fn spliced_local_name(name: &str) -> String {
    if name.starts_with("$i$f$") || name.starts_with("$i$a$") {
        name.to_string()
    } else {
        format!("{name}$iv")
    }
}

/// The name of the inline-depth marker a spliced lambda body opens with.
///
/// kotlinc spells it `$i$a$-<inline callee>-<the lambda's own class>`, and names that class after
/// the declaration the lambda appears in. The lambda is inlined and no such class is emitted, so
/// this string is the only place the name exists.
pub(super) fn spliced_lambda_marker_name(
    callee: &str,
    owner: &str,
    enclosing: &str,
    ordinal: u32,
) -> String {
    // A debug-table name is an UNQUALIFIED name (JVMS 4.2.2): `/` is illegal in one, and a class
    // loader rejects the whole class over it. The owner contributes its simple name only.
    let owner = owner.rsplit('/').next().unwrap_or(owner);
    format!("$i$a$-{callee}-{owner}${enclosing}${}", ordinal + 1)
}

/// Render one source/debug local at the JVM boundary. Common lowering records source spelling,
/// inline depth, and stable lambda identity; only this module owns kotlinc's `$this$`, `$iv`, and
/// `_u24` conventions.
pub(super) fn name(ir: &IrFile, declaration: ExprId) -> Option<String> {
    render(
        ir,
        ir.value_names.get(&declaration).map(String::as_str),
        ir.debug_local_provenance(declaration),
    )
}

/// A name as kotlinc spells it inside a debug local: every character other than a letter, a digit
/// or `_` becomes `_u` and its hexadecimal code (`$` is `_u24`, `-` is `_u2d`).
pub(super) fn escaped(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_alphanumeric() || character == '_' {
            escaped.push(character);
        } else {
            escaped.push_str(&format!("_u{:x}", u32::from(character)));
        }
    }
    escaped
}

/// Render one debug local from the two facts common IR carries for every binding: the source
/// spelling it was declared with, and its provenance.
///
/// A `catch` parameter is declared by its `IrCatch` rather than by a `Variable` node, so it has no
/// declaration expression the two tables could be keyed by and hands the same facts over directly.
pub(super) fn render(
    ir: &IrFile,
    source: Option<&str>,
    provenance: Option<IrDebugLocalProvenance>,
) -> Option<String> {
    match provenance {
        Some(IrDebugLocalProvenance::InlineValue { role, depth }) => {
            let escaped = escaped(source?);
            let mut rendered = match role {
                IrInlineLocalRole::Value => escaped,
                // kotlinc spells the expanded callable's own `this` `this_`, and the receiver it
                // extends `$this$<callable>` — the source name is the callable's in the second case
                // and unused in the first.
                IrInlineLocalRole::DispatchReceiver => "this_".to_string(),
                IrInlineLocalRole::ExtensionReceiver => format!("$this${escaped}"),
            };
            for _ in 0..depth {
                rendered.push_str("$iv");
            }
            Some(rendered)
        }
        Some(IrDebugLocalProvenance::InlineLambdaReceiver { implementation }) => {
            let origin = ir.lambda_origins.get(&implementation)?;
            // kotlinc spells the receiver after the lambda's lifted name.
            let name = ir
                .lifted_names
                .get(&implementation)
                .cloned()
                .unwrap_or_else(|| lambda_implementation_name(origin));
            Some(format!("$this${}", escaped(&name)))
        }
        None => source.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    /// A packaged owner contributes only its simple name: a debug-table name may not contain `/`,
    /// and a class carrying one is rejected at load with `Illegal field name`.
    #[test]
    fn a_spliced_lambda_marker_never_carries_a_qualified_owner() {
        let name =
            super::spliced_lambda_marker_name("firstOrNull", "lib/Catalog", "findResource", 0);
        assert_eq!(name, "$i$a$-firstOrNull-Catalog$findResource$1");
        assert!(!name.contains('/'));
    }

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
    fn inline_extension_receiver_uses_the_jvm_receiver_convention() {
        let mut ir = IrFile::default();
        let declaration = local(&mut ir, "map");
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::inline_value(IrInlineLocalRole::ExtensionReceiver, 1),
        );
        assert_eq!(name(&ir, declaration).as_deref(), Some("$this$map$iv"));
    }

    /// The callable's OWN `this` is a different value from the receiver it extends, and kotlinc
    /// spells it differently. A member inline extension binds both, so one spelling cannot serve.
    #[test]
    fn inline_dispatch_receiver_is_spelled_as_the_callables_own_this() {
        let mut ir = IrFile::default();
        let declaration = local(&mut ir, "send");
        ir.set_debug_local_provenance(
            declaration,
            IrDebugLocalProvenance::inline_value(IrInlineLocalRole::DispatchReceiver, 1),
        );
        assert_eq!(name(&ir, declaration).as_deref(), Some("this_$iv"));
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
