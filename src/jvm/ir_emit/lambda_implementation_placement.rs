//! Physical ownership of lambda implementation methods.
//!
//! Common lowering retains exact implementation identities. This JVM boundary places each private
//! implementation on the class whose bytecode references it, including a source lambda whose
//! implementation is called directly instead of materializing a lambda value.

use std::collections::HashSet;

use crate::ir::{Callee, ExprId, FunId, IrExpr, IrFile};

/// The lambda implementation referenced by one expression.
///
/// A normal lambda value owns an explicit `impl_fn` edge. A lambda implementation detached for a
/// copied inline expansion may instead survive as a direct call. `specialized_functions` records
/// that exact copy; ordinary direct source-lambda calls retain their existing physical owner.
fn implementation_edge(ir: &IrFile, expression: ExprId) -> Option<FunId> {
    match ir.expr(expression) {
        IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
        IrExpr::Call {
            callee: Callee::Local(function),
            ..
        } if ir.specialized_functions.contains_key(function) => Some(*function),
        _ => None,
    }
}

fn reachable_implementations(
    ir: &IrFile,
    roots: Vec<ExprId>,
    include_direct_specializations: bool,
) -> Vec<FunId> {
    let mut implementations = Vec::new();
    let mut found = HashSet::new();
    let mut seen = HashSet::new();
    let mut stack = roots;
    while let Some(expression) = stack.pop() {
        if !seen.insert(expression) {
            continue;
        }
        let implementation = match ir.expr(expression) {
            IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
            _ if include_direct_specializations => implementation_edge(ir, expression),
            _ => None,
        };
        if let Some(implementation) = implementation {
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
    // A direct specialized call is not an implementation-value site. In particular, a consumed
    // inline body may retain that call in a facade DAG while an actual lambda value is emitted from
    // a suspend class. Treating the direct call as facade ownership would veto the class placement
    // required by the lambda metafactory handle and leave the referenced private method absent.
    // Real lambda values remain facade protection, exactly as before direct-call placement existed.
    let facade_reachable = reachable_implementations(ir, facade_roots, false)
        .into_iter()
        .collect::<HashSet<_>>();

    let constructed = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::New { internal, .. } => Some(*internal),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut classes = (0..ir.classes.len()).collect::<Vec<_>>();
    // A copied expression DAG can remain reachable from both its consumed template class and the
    // class that actually emits the copy. The standalone helper belongs to the emitting class. A
    // generated template with no remaining construction must not claim it first: coroutine
    // emission may elide that class and leave the live method handle dangling. Preserve source
    // order within each group so ordinary ownership remains deterministic.
    classes.sort_by_key(|&class| !constructed.contains(&ir.classes[class].fq_name_id()));

    for class_id in classes {
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

        for implementation in reachable_implementations(ir, roots, true) {
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
    fn a_direct_specialized_lambda_call_places_its_implementation_on_the_emitting_class() {
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        let implementation = ir.add_fun(function("lambda", unit, None));
        source_lambda(&mut ir, implementation);
        ir.specialized_functions.insert(
            implementation,
            crate::ir::IrSpecializedFunction {
                source: implementation,
                caller_declaration: crate::fir::DeclarationId::from_raw(0),
                caller: Some(crate::ir::IrEnclosure::File),
                caller_is_default: false,
                caller_source_name: "run".to_string(),
                inline_callee: crate::fir::CallableId::from_raw(0),
                inline_callee_source_name: "inlineCall".to_string(),
                parent: None,
            },
        );

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
    fn an_ordinary_direct_source_lambda_call_keeps_its_existing_owner() {
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

        assert_eq!(ir.classes[class_id as usize].methods, vec![method]);
        assert!(!ir.class_method_owners.contains_key(&implementation));
    }

    #[test]
    fn a_facade_direct_copy_does_not_veto_a_class_lambda_value() {
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        let implementation = ir.add_fun(function("lambda", unit, None));
        source_lambda(&mut ir, implementation);
        ir.specialized_functions.insert(
            implementation,
            crate::ir::IrSpecializedFunction {
                source: implementation,
                caller_declaration: crate::fir::DeclarationId::from_raw(0),
                caller: Some(crate::ir::IrEnclosure::File),
                caller_is_default: false,
                caller_source_name: "run".to_string(),
                inline_callee: crate::fir::CallableId::from_raw(0),
                inline_callee_source_name: "inlineCall".to_string(),
                parent: None,
            },
        );
        let direct = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(implementation),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        ir.add_fun(function("facade", direct, None));
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });
        let owner = crate::types::type_name("sample/Owner");
        let method = ir.add_fun(function("run", lambda, Some(owner)));
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
    fn a_constructed_copy_claims_a_helper_before_an_unconstructed_template() {
        let mut ir = IrFile::default();
        let unit = ir.add_expr(IrExpr::UnitInstance);
        let implementation = ir.add_fun(function("lambda", unit, None));
        source_lambda(&mut ir, implementation);
        let lambda = ir.add_expr(IrExpr::Lambda {
            impl_fn: implementation,
            arity: 0,
            captures: Vec::new(),
            sam: None,
            inline_body: None,
        });

        let template_name = crate::types::type_name("sample/Template");
        let template_method = ir.add_fun(function("run", lambda, Some(template_name)));
        let mut template = crate::plugins::synthetic_class("sample/Template");
        template.methods.push(template_method);
        let template_id = ir.add_class(template);
        ir.note_class_method(template_id, template_method);

        let copy_name = crate::types::type_name("sample/Copy");
        let copy_method = ir.add_fun(function("run", lambda, Some(copy_name)));
        let mut copy = crate::plugins::synthetic_class("sample/Copy");
        copy.methods.push(copy_method);
        let copy_id = ir.add_class(copy);
        ir.note_class_method(copy_id, copy_method);
        ir.add_expr(IrExpr::New {
            internal: copy_name,
            args: Vec::new(),
            ctor_params: None,
            ctor_desc: None,
            external_target: None,
        });

        reparent_lambda_impls(&mut ir);

        assert_eq!(
            ir.classes[template_id as usize].methods,
            vec![template_method]
        );
        assert_eq!(
            ir.classes[copy_id as usize].methods,
            vec![copy_method, implementation]
        );
        assert_eq!(ir.class_method_owners[&implementation], vec![copy_id]);
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
