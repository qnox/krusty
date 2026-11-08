//! Reified uses that require an anonymous class to be copied at an inline call.
use super::property_roots;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{ClassId, ExprId, IrExpr};
use crate::types::{ty_subst_keep_unbound, Ty};

pub(super) fn class_uses_binding(
    ir: &crate::ir::IrFile,
    class: ClassId,
    bindings: &HashMap<String, Ty>,
    seen: &mut HashSet<ClassId>,
) -> bool {
    if bindings.is_empty() || !seen.insert(class) {
        return false;
    }
    let class_decl = ir
        .classes
        .get(class as usize)
        .expect("a constructed source class retains its IR identity");
    if class_decl
        .supertypes
        .iter()
        .any(|ty| ty_subst_keep_unbound(*ty, bindings) != *ty)
    {
        return true;
    }
    let mut functions = class_decl.methods.clone();
    functions.extend(
        class_decl
            .properties
            .iter()
            .flat_map(|property| property.getter.into_iter().chain(property.setter)),
    );
    if let Some(extensions) = ir.member_ext_props.get(&class_decl.fq_name) {
        functions.extend(
            extensions
                .iter()
                .flat_map(|property| std::iter::once(property.getter).chain(property.setter)),
        );
    }
    let mut roots = functions
        .into_iter()
        .filter_map(|function| ir.functions.get(function as usize)?.body)
        .collect::<Vec<_>>();
    roots.extend(class_decl.init_body);
    // Accessor functions are materialized after inlining. Their checked bodies are already
    // expressions on the property, and a reified operation there must still select the copy.
    // A class that is itself a copy keeps those bodies on its specialization record.
    roots.extend(property_roots(ir, class));
    for root in roots {
        if dag_uses_binding(ir, root, bindings, seen) {
            return true;
        }
    }
    false
}

fn dag_uses_binding(
    ir: &crate::ir::IrFile,
    root: ExprId,
    bindings: &HashMap<String, Ty>,
    classes: &mut HashSet<ClassId>,
) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
        if node_uses_binding(ir.expr(expression), bindings)
            || super::super::escaping_lambda::expression_facts_use_binding(ir, expression, bindings)
        {
            return true;
        }
        if let IrExpr::New { internal, .. } = ir.expr(expression) {
            if let Some(class) = ir.class_id_by_name(*internal) {
                if class_uses_binding(ir, class, bindings, classes) {
                    return true;
                }
            }
        }
    }
    false
}

fn node_uses_binding(expression: &IrExpr, bindings: &HashMap<String, Ty>) -> bool {
    let uses = |ty: Ty| ty_subst_keep_unbound(ty, bindings) != ty;
    match expression {
        IrExpr::TypeOp { type_operand, .. } => uses(*type_operand),
        IrExpr::KClassLiteral { classifier, .. } => classifier.is_some_and(uses),
        IrExpr::Call {
            callee: call_callee,
            ..
        } if let crate::ir::Callee::Intrinsic {
            operation: crate::ir::IrIntrinsic::TypeOf { ty },
            ..
        } = &**call_callee =>
        {
            uses(*ty)
        }
        _ => false,
    }
}
