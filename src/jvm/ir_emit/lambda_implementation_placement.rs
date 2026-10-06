//! Physical ownership of lambda implementation methods.
//!
//! Common lowering retains exact implementation identities. This JVM boundary places each private
//! implementation on the class whose bytecode references it, including a source lambda whose
//! implementation is called directly instead of materializing a lambda value.

use std::collections::HashSet;

use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};

/// The lambda implementation referenced by one expression.
///
/// A normal lambda value owns an explicit `impl_fn` edge. An inlined source lambda may instead
/// survive as a direct implementation call. `lambda_origins` is its stable semantic identity.
/// Parameter-layout facts are deliberately
/// insufficient here because callable-reference and function-value adapters publish them too.
fn implementation_edge(ir: &IrFile, expression: ExprId) -> Option<FunId> {
    match ir.expr(expression) {
        IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
        IrExpr::Call {
            callee: Callee::Local(function),
            ..
        } if ir.lambda_origins.contains_key(function) => Some(*function),
        _ => None,
    }
}

fn reachable_implementations(ir: &IrFile, roots: Vec<ExprId>) -> Vec<FunId> {
    let mut implementations = Vec::new();
    let mut found = HashSet::new();
    let mut seen = HashSet::new();
    let mut stack = roots;
    while let Some(expression) = stack.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let Some(implementation) = implementation_edge(ir, expression) {
            if found.insert(implementation) {
                implementations.push(implementation);
                // Nested lambda implementations emit wherever their owning implementation does.
                if let Some(body) = ir
                    .functions
                    .get(implementation as usize)
                    .and_then(|function| function.body)
                {
                    stack.push(body);
                }
            }
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| stack.push(child));
    }
    implementations
}

/// Reparent lambda implementation methods into the class whose code references them.
///
/// An implementation is private, so both an `invokedynamic` method handle and a direct source-
/// lambda call must name a method on their emitting class. Lowering attaches ordinary class
/// members eagerly; this pass owns code that reaches a class only later, including enum-entry
/// arguments and suspend-lambda state-machine bodies.
pub(crate) fn reparent_lambda_impls(ir: &mut IrFile) {
    let mut owned: HashSet<FunId> = ir
        .classes
        .iter()
        .flat_map(|class| class.methods.iter().copied())
        .collect();

    // An implementation referenced by facade code must stay there. A suspend machine can share an
    // expression DAG with the facade, so the class walk alone is not sufficient evidence to move it.
    let mut facade_roots = Vec::new();
    for (index, function) in ir.functions.iter().enumerate() {
        let function_id = index as FunId;
        if !owned.contains(&function_id)
            && function.dispatch_receiver.is_none()
            && !ir.lambda_own_params_from.contains_key(&function_id)
        {
            facade_roots.extend(function.body);
        }
    }
    for property in &ir.statics {
        facade_roots.extend(property.init);
    }
    let facade_reachable = reachable_implementations(ir, facade_roots)
        .into_iter()
        .collect::<HashSet<_>>();

    for class_id in 0..ir.classes.len() {
        let class = &ir.classes[class_id];
        let mut roots = Vec::new();
        for &function in &class.methods {
            roots.extend(
                ir.functions
                    .get(function as usize)
                    .and_then(|shape| shape.body),
            );
        }
        roots.extend(class.init_body);
        roots.extend(ir.companion_clinit_body(class.fq_name));
        roots.extend(class.super_arg_prelude.iter().copied());
        roots.extend(class.super_args.iter().copied());
        for constructor in &class.secondary_ctors {
            roots.extend(constructor.body);
            roots.extend(constructor.defaults.iter().flatten().copied());
            roots.extend(constructor.delegate_prelude.iter().copied());
            roots.extend(constructor.delegate_args.iter().copied());
        }
        for entry in &class.enum_entries {
            roots.extend(entry.args.iter().copied());
        }

        for implementation in reachable_implementations(ir, roots) {
            // Only a free standalone implementation moves. A spliced inline-only implementation
            // has no method, and one referenced by facade code must remain accessible there.
            if !owned.contains(&implementation)
                && !facade_reachable.contains(&implementation)
                && !ir.inline_only_fns.contains(&implementation)
                && ir
                    .functions
                    .get(implementation as usize)
                    .is_some_and(|function| function.dispatch_receiver.is_none())
            {
                owned.insert(implementation);
                ir.classes[class_id].methods.push(implementation);
                ir.note_class_method(class_id as u32, implementation);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::reparent_lambda_impls;
    use crate::ir::{Callee, IrExpr, IrFile, IrFunction, IrLambdaForm, IrLambdaOrigin};
    use crate::types::{Ty, TypeName};

    fn function(name: &str, body: u32, dispatch_receiver: Option<TypeName>) -> IrFunction {
        IrFunction {
            name: name.to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(body),
            is_static: dispatch_receiver.is_none(),
            dispatch_receiver,
            param_checks: Vec::new(),
        }
    }

    fn source_lambda(ir: &mut IrFile, implementation: u32) {
        ir.lambda_origins.insert(
            implementation,
            IrLambdaOrigin {
                identity: 0,
                lexical_owner: None,
                enclosing_name: "box".to_string(),
                binding_name: None,
                ordinal: 0,
                implementation_name: "box".to_string(),
                implementation_ordinal: 0,
                receiver_parameter: None,
                label: None,
                form: IrLambdaForm::Literal,
                class_provenance: None,
            },
        );
    }

    #[test]
    fn a_direct_source_lambda_call_places_its_implementation_on_the_emitting_class() {
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        let implementation = ir.add_fun(function("lambda", unit, None));
        source_lambda(&mut ir, implementation);

        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(implementation),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let owner = crate::types::type_name("sample/Owner");
        let method = ir.add_fun(function("run", call, Some(owner)));
        let mut class = crate::plugins::synthetic_class("sample/Owner");
        class.methods.push(method);
        let class_id = ir.add_class(class);
        ir.note_class_method(class_id, method);

        reparent_lambda_impls(&mut ir);

        assert_eq!(
            ir.classes[class_id as usize].methods,
            vec![method, implementation]
        );
        assert_eq!(ir.class_method_owners[&implementation], vec![class_id]);
    }

    #[test]
    fn a_direct_adapter_call_does_not_move_from_the_facade() {
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        let adapter = ir.add_fun(function("adapter", unit, None));
        ir.lambda_own_params_from.insert(adapter, 0);

        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(adapter),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        let owner = crate::types::type_name("sample/Owner");
        let method = ir.add_fun(function("run", call, Some(owner)));
        let mut class = crate::plugins::synthetic_class("sample/Owner");
        class.methods.push(method);
        let class_id = ir.add_class(class);
        ir.note_class_method(class_id, method);

        reparent_lambda_impls(&mut ir);

        assert_eq!(ir.classes[class_id as usize].methods, vec![method]);
        assert!(!ir.class_method_owners.contains_key(&adapter));
    }
}
