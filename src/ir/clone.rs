//! Structural cloning for one common-IR expression DAG.
//!
//! Inline expansion needs a private copy of a retained body. Keeping the arena plumbing here gives
//! every consumer one exhaustive child remapper instead of growing another ad-hoc expression walker.

use std::collections::HashMap;

use super::{ExprId, FunId, IrCheckedArgument, IrCheckedOperation, IrExpr, IrFile, IrFunction};
use crate::types::{ty_subst_keep_unbound, Ty};

/// Clone `root` and every reachable expression, preserving sparse semantic/source facts. Returns
/// the new root and the complete old-to-new identity map.
pub fn clone_expression_dag(ir: &mut IrFile, root: ExprId) -> (ExprId, HashMap<ExprId, ExprId>) {
    fn clone_one(ir: &mut IrFile, source: ExprId, cloned: &mut HashMap<ExprId, ExprId>) -> ExprId {
        if let Some(&existing) = cloned.get(&source) {
            return existing;
        }
        let mut children = Vec::new();
        super::for_each_child(&ir.exprs, source, &mut |child| children.push(child));
        for child in children {
            clone_one(ir, child, cloned);
        }
        let mut expression = ir.exprs[source as usize].clone();
        remap_direct_children(&mut expression, |child| cloned[&child]);
        let target = ir.add_expr(expression);
        copy_expression_facts(ir, source, target);
        cloned.insert(source, target);
        target
    }

    let mut cloned = HashMap::new();
    let root = clone_one(ir, root, &mut cloned);
    (root, cloned)
}

/// Give every child edge below `root` its own expression nodes while retaining `root` itself.
///
/// Common IR is a DAG: one operand may deliberately be referenced by several parents. A transform
/// that rewrites descendants in place must first make each USE private, or a rewrite reached through
/// one parent also changes the other parent without putting its prerequisites in scope. Keeping the
/// root identity lets callers preserve function-body and side-table ownership while all mutable
/// descendants become a tree.
#[cfg(test)]
pub(crate) fn make_expression_children_unique(ir: &mut IrFile, root: ExprId) {
    let _ = make_expression_children_unique_tracked(ir, root);
}

/// As [`make_expression_children_unique`], returning every `(source, clone)` identity so backend
/// side tables can copy their exact per-expression contracts instead of inferring them again.
pub(crate) fn make_expression_children_unique_tracked(
    ir: &mut IrFile,
    root: ExprId,
) -> Vec<(ExprId, ExprId)> {
    fn clone_use(ir: &mut IrFile, source: ExprId, clones: &mut Vec<(ExprId, ExprId)>) -> ExprId {
        let mut expression = ir.exprs[source as usize].clone();
        remap_direct_children(&mut expression, |child| clone_use(ir, child, clones));
        let target = ir.add_expr(expression);
        copy_expression_facts(ir, source, target);
        clones.push((source, target));
        target
    }

    crate::wide_stack::on_wide_stack(|| {
        let mut clones = Vec::new();
        let mut expression = ir.exprs[root as usize].clone();
        remap_direct_children(&mut expression, |child| clone_use(ir, child, &mut clones));
        ir.exprs[root as usize] = expression;
        clones
    })
}

fn map_option(value: &mut Option<ExprId>, map: &mut impl FnMut(ExprId) -> ExprId) {
    if let Some(value) = value {
        *value = map(*value);
    }
}

fn remap_argument(argument: &mut IrCheckedArgument, map: &mut impl FnMut(ExprId) -> ExprId) {
    match argument {
        IrCheckedArgument::Expression { value, .. } => *value = map(*value),
        IrCheckedArgument::Default { .. } => {}
        IrCheckedArgument::Vararg { elements, .. } => {
            for (value, _) in elements {
                *value = map(*value);
            }
        }
    }
}

/// Rewrite every direct child identity. Exhaustive by design: adding an `IrExpr` shape must update
/// both this function and `for_each_child` before the compiler builds.
fn remap_direct_children(expression: &mut IrExpr, mut map: impl FnMut(ExprId) -> ExprId) {
    match expression {
        IrExpr::Checked(operation) => match operation {
            IrCheckedOperation::Call {
                dispatch_receiver,
                extension_receiver,
                arguments,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                map_option(extension_receiver, &mut map);
                arguments
                    .iter_mut()
                    .for_each(|argument| remap_argument(argument, &mut map));
            }
            IrCheckedOperation::ConstructorDelegation {
                outer_receiver,
                arguments,
                ..
            } => {
                map_option(outer_receiver, &mut map);
                arguments
                    .iter_mut()
                    .for_each(|argument| remap_argument(argument, &mut map));
            }
            IrCheckedOperation::BackingFieldRead {
                dispatch_receiver, ..
            }
            | IrCheckedOperation::LateinitFieldRead {
                dispatch_receiver, ..
            } => map_option(dispatch_receiver, &mut map),
            IrCheckedOperation::BackingFieldWrite {
                dispatch_receiver,
                value,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                *value = map(*value);
            }
            IrCheckedOperation::PropertyRead {
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                map_option(extension_receiver, &mut map);
                context_arguments
                    .iter_mut()
                    .for_each(|value| *value = map(*value));
            }
            IrCheckedOperation::PropertyWrite {
                dispatch_receiver,
                extension_receiver,
                context_arguments,
                value,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                map_option(extension_receiver, &mut map);
                context_arguments
                    .iter_mut()
                    .for_each(|value| *value = map(*value));
                *value = map(*value);
            }
            IrCheckedOperation::ExternalPropertyRead {
                receiver,
                arguments,
                ..
            }
            | IrCheckedOperation::ExternalPropertyWrite {
                receiver,
                arguments,
                ..
            } => {
                map_option(receiver, &mut map);
                arguments.iter_mut().for_each(|value| *value = map(*value));
            }
            IrCheckedOperation::RangeConstruction { start, end, .. } => {
                *start = map(*start);
                *end = map(*end);
            }
            IrCheckedOperation::IllegalProgressionStep { step } => *step = map(*step),
            IrCheckedOperation::RangeContains {
                value, start, end, ..
            } => {
                *value = map(*value);
                *start = map(*start);
                *end = map(*end);
            }
            IrCheckedOperation::RangeLoop {
                source,
                body,
                with_index,
                ..
            } => {
                source.map_operands(&mut map);
                if let Some(index) = with_index {
                    index.declaration = map(index.declaration);
                    index.bindings = map(index.bindings);
                    index.element_copies = map(index.element_copies);
                }
                *body = map(*body);
            }
            IrCheckedOperation::PropertyReference {
                dispatch_receiver,
                extension_receiver,
                ..
            } => {
                map_option(dispatch_receiver, &mut map);
                map_option(extension_receiver, &mut map);
            }
        },
        IrExpr::CallableReference(reference) => {
            reference
                .captures
                .iter_mut()
                .for_each(|capture| *capture = map(*capture));
            map_option(&mut reference.bound_receiver, &mut map);
        }
        IrExpr::Block { stmts, value } => {
            stmts.iter_mut().for_each(|value| *value = map(*value));
            map_option(value, &mut map);
        }
        IrExpr::When { branches } => branches.iter_mut().for_each(|(condition, body)| {
            map_option(condition, &mut map);
            *body = map(*body);
        }),
        IrExpr::Return(value) => map_option(value, &mut map),
        IrExpr::BottomValue { producer: arg, .. }
        | IrExpr::TypeOp { arg, .. }
        | IrExpr::NotNullAssert { operand: arg, .. }
        | IrExpr::LateinitCheck { operand: arg, .. }
        | IrExpr::Throw { operand: arg }
        | IrExpr::EnumValueOf { arg, .. }
        | IrExpr::ReifiedTypeOp { arg, .. }
        | IrExpr::RefGet { holder: arg, .. }
        | IrExpr::NewArray { size: arg, .. }
        | IrExpr::PrimitiveNeg { operand: arg, .. } => *arg = map(*arg),
        IrExpr::StringConcat(values)
        | IrExpr::New { args: values, .. }
        | IrExpr::Vararg {
            elements: values, ..
        } => values.iter_mut().for_each(|value| *value = map(*value)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } | IrExpr::Equality { lhs, rhs, .. } => {
            *lhs = map(*lhs);
            *rhs = map(*rhs);
        }
        IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => *value = map(*value),
        IrExpr::SetField {
            receiver, value, ..
        }
        | IrExpr::RefSet {
            holder: receiver,
            value,
            ..
        } => {
            *receiver = map(*receiver);
            *value = map(*value);
        }
        IrExpr::PropertyWrite {
            receiver, value, ..
        } => {
            map_option(receiver, &mut map);
            *value = map(*value);
        }
        IrExpr::Variable { init, .. } | IrExpr::RefNew { init, .. } => map_option(init, &mut map),
        IrExpr::EnclosingInstance { receiver, .. }
        | IrExpr::GetField { receiver, .. }
        | IrExpr::LateinitInitialized { receiver, .. } => *receiver = map(*receiver),
        IrExpr::PropertyRead { receiver, .. } => map_option(receiver, &mut map),
        IrExpr::Call {
            args,
            dispatch_receiver,
            ..
        } => {
            map_option(dispatch_receiver, &mut map);
            args.iter_mut().for_each(|value| *value = map(*value));
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            *receiver = map(*receiver);
            args.iter_mut()
                .flatten()
                .for_each(|value| *value = map(*value));
        }
        IrExpr::InvokeFunction { func, args, .. } => {
            *func = map(*func);
            args.iter_mut().for_each(|value| *value = map(*value));
        }
        IrExpr::Lambda {
            captures,
            inline_body,
            ..
        } => {
            captures.iter_mut().for_each(|value| *value = map(*value));
            map_option(inline_body, &mut map);
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            *cond = map(*cond);
            *body = map(*body);
            map_option(update, &mut map);
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            *body = map(*body);
            catches
                .iter_mut()
                .for_each(|catch| catch.body = map(catch.body));
            map_option(finally, &mut map);
        }
        IrExpr::PluginPlaceholder { exprs, .. } => {
            exprs.iter_mut().for_each(|value| *value = map(*value));
        }
        IrExpr::KClassLiteral { value, .. } => map_option(value, &mut map),
        IrExpr::LocalDelegateAccess(access) => {
            access.delegate = map(access.delegate);
            map_option(&mut access.dispatch_receiver, &mut map);
            map_option(&mut access.value, &mut map);
        }
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::LocalPropertyReference(_)
        | IrExpr::SingletonValue { .. }
        | IrExpr::GetValue(_)
        | IrExpr::ForwardedSuperArgument { .. }
        | IrExpr::GetStatic(_)
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::EnumEntry { .. }
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::EnumValues { .. }
        | IrExpr::EnumEntries { .. }
        | IrExpr::ReifiedClassMarker { .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation
        | IrExpr::InlineFrameMarker => {}
    }
}

fn copy_expression_facts(ir: &mut IrFile, source: ExprId, target: ExprId) {
    macro_rules! copy_map {
        ($field:ident) => {
            if let Some(value) = ir.$field.get(&source).cloned() {
                ir.$field.insert(target, value);
            }
        };
    }
    ir.copy_generated_line_marks(source, target);
    ir.copy_inline_copy_mark(source, target);
    copy_map!(fir_origins);
    copy_map!(expression_owners);
    copy_map!(callable_reference_provenance);
    copy_map!(callable_reference_enclosures);
    copy_map!(checked_return_depths);
    copy_map!(annotation_constructions);
    copy_map!(generated_secondary_constructor_calls);
    copy_map!(expr_lines);
    copy_map!(expr_source_lines);
    copy_map!(expr_end_lines);
    copy_map!(value_names);
    copy_map!(logical_types);
    copy_map!(deferred_local_types);
    if let Some(result) = ir.whens.exhaustive.get(&source).copied() {
        ir.whens.exhaustive.insert(target, result);
    }
    if let Some(line) = ir.whens.source_lines.get(&source).copied() {
        ir.whens.source_lines.insert(target, line);
    }
    copy_map!(binding_read_stability);
    copy_map!(short_circuits);
    copy_map!(physical_types);
    copy_map!(reified_call_subst);
    copy_map!(reified_catch_markers);
    copy_map!(inline_call_type_arguments);
    copy_map!(ext_call_source_receiver);
    copy_map!(dispatch_classes);
    copy_map!(call_declared_ret);
    copy_map!(synthesized_accessor_calls);
    copy_map!(call_declared_params);
    copy_map!(module_member_accesses);
    copy_map!(jvm_protected_dependency_calls);
    copy_map!(construction_declared_params);
    copy_map!(construction_targets);
    copy_map!(static_extension_receivers);
    copy_map!(call_inline_modifiers);
    copy_map!(suspend_calls);
    copy_map!(suspend_call_overridden_results);
    copy_map!(value_class_suspend_calls);
    copy_map!(intrinsic_suspension_points);
    copy_map!(erased_value_constructions);
    if let Some(provenance) = ir.debug_local_provenance(source) {
        ir.set_debug_local_provenance(target, provenance);
    }
    if ir.property_initializer_stores.contains(&source) {
        ir.property_initializer_stores.insert(target);
    }
    if ir.inline_regions.contains(&source) {
        ir.inline_regions.insert(target);
    }
    if ir.callable_scopes.contains(&source) {
        ir.callable_scopes.insert(target);
    }
    if ir.call_operand_bindings.contains(&source) {
        ir.call_operand_bindings.insert(target);
    }
    if ir.null_guards.contains(&source) {
        ir.null_guards.insert(target);
    }
    if ir.plain_updates.contains(&source) {
        ir.plain_updates.insert(target);
    }
    if ir.transparent_loop_bodies.contains(&source) {
        ir.transparent_loop_bodies.insert(target);
    }
    if ir.for_loop_next_loops.contains(&source) {
        ir.for_loop_next_loops.insert(target);
    }
    if ir.lateinit_initialization_probes.contains(&source) {
        ir.lateinit_initialization_probes.insert(target);
    }
    if ir.written_casts.contains(&source) {
        ir.written_casts.insert(target);
    }
    if ir.declaration_result_coercions.contains(&source) {
        ir.declaration_result_coercions.insert(target);
    }
    if let Some(boundaries) = ir.declaration_argument_boundaries.get(&source).cloned() {
        let supplied = |expression: ExprId| match ir.expr(expression) {
            IrExpr::Call { args, .. } => args.clone(),
            IrExpr::MethodCall { args, .. } => args.iter().copied().flatten().collect(),
            _ => Vec::new(),
        };
        let source_arguments = supplied(source);
        let target_arguments = supplied(target);
        let boundaries = boundaries
            .iter()
            .map(|boundary| {
                let position = source_arguments
                    .iter()
                    .position(|argument| *argument == boundary.argument)
                    .expect("recorded declaration argument belongs to cloned call");
                let mut boundary = *boundary;
                boundary.argument = target_arguments[position];
                boundary
            })
            .collect::<Vec<_>>();
        ir.declaration_argument_boundaries
            .insert(target, boundaries.into_boxed_slice());
    }
    if let Some(parameters) = ir.physical_call_parameters.get(&source).cloned() {
        ir.physical_call_parameters.insert(target, parameters);
    }
    if ir.elvis_safe_call_guards.contains(&source) {
        ir.elvis_safe_call_guards.insert(target);
    }
    if ir.inline_call_sites.contains(&source) {
        ir.inline_call_sites.insert(target);
    }
    if ir.module_inline_calls.contains(&source) {
        ir.module_inline_calls.insert(target);
    }
}

/// Copy one lambda implementation and the facts that belong to that implementation.
///
/// This is not a general function clone. Declaration identity stays on `source`: the callable map,
/// the facade slot, and the inline-declaration sets. Target realization tables stay empty so a
/// later backend pass records the copy itself. `specialization` is the expansion that created the
/// copy. Classes that already list `source` are read from [`IrFile::class_method_owners`].
pub(crate) fn clone_function_implementation(
    ir: &mut IrFile,
    source: FunId,
    shape: IrFunction,
    bindings: &HashMap<String, Ty>,
    specialization: super::IrSpecializedFunction,
) -> FunId {
    let owners = ir
        .class_method_owners
        .get(&source)
        .cloned()
        .unwrap_or_default();
    let target = ir.add_fun(shape);
    ir.specialized_functions.insert(
        target,
        super::IrSpecializedFunction {
            source,
            ..specialization
        },
    );
    copy_function_implementation_facts(ir, source, target, bindings);
    for class in owners {
        ir.classes[class as usize].methods.push(target);
        ir.class_method_owners
            .entry(target)
            .or_default()
            .push(class);
    }
    target
}

/// Copy one class method without attaching it to the source class or recording it as a specialized
/// lambda. The caller publishes it on the class copy.
pub(crate) fn clone_class_method(
    ir: &mut IrFile,
    source: FunId,
    shape: IrFunction,
    bindings: &HashMap<String, Ty>,
) -> FunId {
    let target = ir.add_fun(shape);
    copy_function_implementation_facts(ir, source, target, bindings);
    target
}

fn copy_function_implementation_facts(
    ir: &mut IrFile,
    source: FunId,
    target: FunId,
    bindings: &HashMap<String, Ty>,
) {
    macro_rules! copy_map {
        ($field:ident) => {
            if let Some(value) = ir.$field.get(&source).cloned() {
                ir.$field.insert(target, value);
            }
        };
    }
    macro_rules! copy_set {
        ($field:ident) => {
            if ir.$field.contains(&source) {
                ir.$field.insert(target);
            }
        };
    }

    ir.set_method_visibility(target, ir.method_visibility(source));
    copy_map!(fn_source_names);
    copy_map!(fn_params);
    copy_map!(fn_param_declared_nullable);
    copy_map!(fn_declared_spellings);
    copy_map!(fn_context_counts);
    copy_map!(fn_param_annotations);
    copy_map!(fn_param_no_infer);
    copy_map!(fn_return_value_statuses);
    copy_map!(fn_varargs);
    copy_map!(fn_decl_lines);
    copy_map!(fn_close_lines);
    copy_map!(fn_sig_lines);
    copy_map!(fn_signature_offsets);
    copy_map!(fn_source_order);
    copy_map!(fn_continuation_ordinal);
    copy_set!(fn_debug_locals);
    copy_map!(lambda_own_params_from);
    copy_map!(lambda_enclosures);
    copy_map!(lambda_sam_signature);
    if let Some((parameters, result)) = ir.lambda_sam_signature.get_mut(&target) {
        for parameter in parameters.iter_mut() {
            *parameter = ty_subst_keep_unbound(*parameter, bindings);
        }
        *result = ty_subst_keep_unbound(*result, bindings);
    }
    copy_map!(lifted_functions);
    copy_map!(lifted_names);
    copy_map!(function_annotations);
    copy_map!(member_semantic_sigs);
    if let Some((parameters, result)) = ir.member_semantic_sigs.get_mut(&target) {
        for parameter in parameters.iter_mut() {
            *parameter = ty_subst_keep_unbound(*parameter, bindings);
        }
        *result = ty_subst_keep_unbound(*result, bindings);
    }
    copy_map!(signatures);
    if let Some(signature) = ir.signatures.get_mut(&target) {
        for parameter in &mut signature.params {
            *parameter = ty_subst_keep_unbound(*parameter, bindings);
        }
        if let Some(result) = signature.ret.as_mut() {
            *result = ty_subst_keep_unbound(*result, bindings);
        }
        for supertype in &mut signature.supers {
            *supertype = ty_subst_keep_unbound(*supertype, bindings);
        }
        for parameter in &mut signature.type_params {
            for (bound, _) in &mut parameter.bounds {
                *bound = ty_subst_keep_unbound(*bound, bindings);
            }
        }
    }
    copy_map!(callable_bound_type_parameters);
    if let Some(parameters) = ir.callable_bound_type_parameters.get_mut(&target) {
        for parameter in parameters {
            for (bound, _) in &mut parameter.bounds {
                *bound = ty_subst_keep_unbound(*bound, bindings);
            }
        }
    }
    copy_map!(suspend_declared_sigs);
    if let Some((parameters, result)) = ir.suspend_declared_sigs.get_mut(&target) {
        for parameter in parameters.iter_mut() {
            *parameter = ty_subst_keep_unbound(*parameter, bindings);
        }
        *result = ty_subst_keep_unbound(*result, bindings);
    }
    copy_map!(value_class_suspend_returns);
    copy_set!(extension_receiver_fns);
    copy_set!(function_typed_parameter_fns);
    copy_set!(operator_fns);
    copy_set!(infix_fns);
    copy_set!(tailrec_fns);
    copy_set!(open_methods);
    copy_set!(must_inline_lambdas);
    copy_set!(synthetic_methods);
    copy_set!(bridge_methods);
    copy_set!(deprecated_methods);
    copy_set!(inline_only_fns);
    copy_set!(unlooped_tailrec);
    if let Some(owner) = ir.class_static_local_functions.get(&source).copied() {
        ir.class_static_local_functions.insert(target, owner);
    }
    copy_set!(serialization_cache_methods);
    if ir.suspend_funs.contains(&source) {
        ir.suspend_funs.push(target);
    }
    ir.copy_lambda_type_parameters(source, target, bindings);
    ir.copy_lambda_class_provenance(source, target);
    // A specialization is another implementation of the same source lambda. Identity, ordinals, and
    // class provenance stay on that source record; [`super::IrSpecializedFunction`] is the generated
    // copy's identity.
    if let Some(origin) = ir.lambda_origins.get(&source).cloned() {
        ir.lambda_origins.insert(target, origin);
    }
    let captures = ir
        .shared_capture_parameters
        .iter()
        .filter(|((function, _), _)| *function == source)
        .map(|((_, ordinal), ty)| (*ordinal, *ty))
        .collect::<Vec<_>>();
    for (ordinal, ty) in captures {
        ir.shared_capture_parameters
            .insert((target, ordinal), ty_subst_keep_unbound(ty, bindings));
    }
    // Left on the source. These name the declaration, not one specialized implementation of it:
    // `checked_callable_functions`, `top_level_function_fids`, `public_inline_functions`,
    // `top_level_inline_functions`, `inline_fns`, `foreign_inline_templates`,
    // `function_reference_access_bridges`, and `interface_delegation_forwarders`.
    // JVM realization tables (`jvm_*`, `lambda_sam_jvm_signature`, `lambda_class_names`,
    // `generated_static_facts`, `vc_declared_sigs`, `default_stub_boxed_params`) stay empty so the
    // target pass that owns them records the copy.
}

#[cfg(test)]
mod tests {
    use super::super::test_support::blank_class;
    use super::super::{
        IrEnclosure, IrFile, IrFunction, IrGenericSig, IrSpecializedFunction, IrTypeParameter,
    };
    use super::{clone_expression_dag, clone_function_implementation};
    use crate::types::{Ty, Visibility};

    fn function(name: &str, parameter: Ty) -> IrFunction {
        IrFunction {
            name: name.to_string(),
            params: vec![parameter],
            ret: Ty::obj("kotlin/Any"),
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        }
    }

    fn signature(parameter: Ty) -> IrGenericSig {
        IrGenericSig {
            type_params: vec![IrTypeParameter {
                name: "T".to_string(),
                semantic_name: "T".to_string(),
                bounds: vec![(Ty::obj("kotlin/Any"), false)],
                variance: Default::default(),
                reified: true,
            }],
            params: vec![parameter],
            ret: Some(parameter),
            supers: Vec::new(),
        }
    }

    #[test]
    fn expression_clone_keeps_full_inline_arguments_distinct_from_reified_arguments() {
        let mut ir = IrFile::default();
        let source = ir.add_expr(super::super::IrExpr::UnitInstance);
        let full = vec![("T".to_string(), Ty::String), ("R".to_string(), Ty::Int)];
        let reified = vec![("R".to_string(), Ty::Int)];
        ir.inline_call_type_arguments.insert(source, full.clone());
        ir.reified_call_subst.insert(source, reified.clone());

        let (target, copies) = clone_expression_dag(&mut ir, source);

        assert_eq!(copies, std::collections::HashMap::from([(source, target)]));
        assert_eq!(ir.inline_call_type_arguments.len(), 2);
        assert_eq!(ir.inline_call_type_arguments.get(&source), Some(&full));
        assert_eq!(ir.inline_call_type_arguments.get(&target), Some(&full));
        assert_eq!(ir.reified_call_subst.len(), 2);
        assert_eq!(ir.reified_call_subst.get(&source), Some(&reified));
        assert_eq!(ir.reified_call_subst.get(&target), Some(&reified));
    }

    #[test]
    fn clone_function_implementation_copies_facts_and_keeps_the_declaration() {
        let mut ir = IrFile::default();
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let source = ir.add_fun(function("check", parameter));
        ir.set_method_visibility(source, Visibility::Private);
        ir.fn_decl_lines.insert(source, 4);
        ir.inline_fns.insert(source);
        ir.signatures.insert(source, signature(parameter));
        ir.callable_bound_type_parameters.insert(
            source,
            vec![IrTypeParameter {
                name: "C".to_string(),
                semantic_name: "C".to_string(),
                bounds: vec![(parameter, false)],
                variance: Default::default(),
                reified: false,
            }],
        );
        ir.record_lambda_type_parameters(source, vec![signature(parameter).type_params[0].clone()]);
        ir.record_lambda_class_provenance(
            source,
            crate::ir::type_reflection::LambdaClassProvenance::SourceFunction,
        );
        let class = ir.add_class(blank_class("Holder"));
        ir.classes[class as usize].methods.push(source);
        ir.note_class_method(class, source);
        let caller = ir.add_fun(function("box", Ty::obj("kotlin/Int")));
        let specialized = Ty::obj("sample/Token");
        let bindings = std::collections::HashMap::from([("T".to_string(), specialized)]);

        let first = clone_function_implementation(
            &mut ir,
            source,
            function("check", specialized),
            &bindings,
            IrSpecializedFunction {
                source: caller,
                caller_declaration: crate::fir::DeclarationId::from_raw(7),
                caller: Some(IrEnclosure::Function(caller)),
                caller_is_default: false,
                caller_source_name: "box".to_string(),
                inline_callee: crate::fir::CallableId::from_raw(11),
                inline_callee_source_name: "defineFunc".to_string(),
                parent: None,
            },
        );
        let second = clone_function_implementation(
            &mut ir,
            source,
            function("check", specialized),
            &bindings,
            IrSpecializedFunction {
                source,
                caller_declaration: crate::fir::DeclarationId::from_raw(7),
                caller: Some(IrEnclosure::Function(caller)),
                caller_is_default: false,
                caller_source_name: "box".to_string(),
                inline_callee: crate::fir::CallableId::from_raw(11),
                inline_callee_source_name: "defineFunc".to_string(),
                parent: None,
            },
        );

        assert_ne!(first, source);
        assert_ne!(second, first);
        assert_eq!(ir.functions[source as usize].params, vec![parameter]);
        assert_eq!(ir.functions[first as usize].params, vec![specialized]);
        assert_eq!(ir.method_visibility(first), Visibility::Private);
        assert_eq!(ir.fn_decl_lines.get(&first), Some(&4));
        assert!(ir.inline_fns.contains(&source));
        assert!(!ir.inline_fns.contains(&first));
        assert!(!ir.specialized_functions.contains_key(&source));
        assert_eq!(
            ir.classes[class as usize].methods,
            vec![source, first, second]
        );
        assert_eq!(ir.signatures[&source].params, vec![parameter]);
        assert_eq!(ir.signatures[&source].ret, Some(parameter));
        assert_eq!(ir.signatures[&first].params, vec![specialized]);
        assert_eq!(ir.signatures[&first].ret, Some(specialized));
        assert_eq!(
            ir.callable_bound_type_parameters[&source][0].bounds[0].0,
            parameter
        );
        assert_eq!(
            ir.callable_bound_type_parameters[&first][0].bounds[0].0,
            specialized
        );
        assert_eq!(
            ir.lambda_class_provenance(first),
            Some(crate::ir::type_reflection::LambdaClassProvenance::SourceFunction)
        );
        assert_eq!(ir.lambda_type_parameters(first).len(), 1);
        assert_eq!(
            ir.lambda_type_parameters(first)[0].bounds[0].0,
            Ty::obj("kotlin/Any")
        );
        let recorded = &ir.specialized_functions[&first];
        assert_eq!(recorded.source, source);
        assert_eq!(recorded.caller, Some(IrEnclosure::Function(caller)));
        assert_eq!(recorded.inline_callee, crate::fir::CallableId::from_raw(11));
        assert_eq!(recorded.inline_callee_source_name, "defineFunc");
        assert_eq!(recorded.parent, None);
        assert_eq!(ir.specialized_functions[&second].parent, None);
        assert_eq!(ir.class_method_owners.get(&first), Some(&vec![class]));
    }
}
