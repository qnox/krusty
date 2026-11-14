//! A target-neutral continuation object for a suspend function whose body can suspend.

use crate::fx_hash::HashSet;
use crate::ir::{
    Callee, ClassId, ExprId, IrCatch, IrClass, IrCtorArg, IrCtorParameterProvenance, IrEnclosure,
    IrExpr, IrField, IrFile, IrFunction, IrFunctionOverride, IrGeneratedParameterRole,
    IrGenericSig, IrLocalClassNameProvenance, IrModuleSource, IrParameterIdentity,
    IrPropertyOverride, IrTypeOp, IrTypeParameter,
};
use crate::types::{type_name, wk, Ty, TypeName};

use super::class_storage::{
    external_type_substitution, plan_frame_values, realize_frame_values, ContinuationFields,
};
use super::label_reentry::{split_at_suspensions, ReentryDecline};
use super::value_namespace::max_value_index;

#[derive(Clone, Copy)]
struct ResumeContract {
    class: ClassId,
    runtime: crate::ir::IrCoroutineRuntime,
    machine_result_ty: Ty,
    completion_result_ty: Ty,
    function_result: Ty,
    any: Ty,
    completion_field: u32,
    result_field: u32,
}

/// Lower one CPS-shaped suspend body into a generated continuation class and a wrapper entry.
pub(super) fn realize(
    ir: &mut IrFile,
    function: u32,
    suspend_functions: &HashSet<u32>,
) -> Result<(), ReentryDecline> {
    let Some(body) = ir.functions[function as usize].body else {
        return Ok(());
    };
    let runtime = ir.coroutine_runtime.ok_or_else(|| {
        crate::trace_compiler!(
            "suspend",
            "state-machine function={function} name={:?} has no selected coroutine runtime",
            ir.functions[function as usize].name,
        );
        ReentryDecline::UnsupportedCallShape
    })?;
    let (source_parameters, function_result) = ir
        .suspend_declared_sigs
        .get(&function)
        .cloned()
        .ok_or_else(|| {
            crate::trace_compiler!(
                "suspend",
                "state-machine function={function} name={:?} has no source declared signature",
                ir.functions[function as usize].name,
            );
            ReentryDecline::UnsupportedCallShape
        })?;
    let source_values = super::value_namespace::function_value_types(ir, function, body);
    let type_parameters = machine_type_parameters(ir, function);
    let parameter_names = type_parameters
        .iter()
        .map(|parameter| parameter.semantic_name.clone())
        .collect::<Vec<_>>();
    let has_unowned_type_parameter = source_parameters
        .iter()
        .copied()
        .chain(std::iter::once(function_result))
        .chain(source_values.values().copied())
        .any(|ty| crate::types::ty_mentions_undeclared_param(ty, &parameter_names))
        || type_parameters.iter().any(|parameter| {
            parameter.bounds.iter().any(|(bound, _)| {
                crate::types::ty_mentions_undeclared_param(*bound, &parameter_names)
            })
        });
    if has_unowned_type_parameter {
        crate::trace_compiler!(
            "suspend",
            "state-machine function={function} name={:?} has unowned type parameter: source_parameters={source_parameters:?} result={function_result:?} source_values={source_values:?} machine_parameters={type_parameters:?}",
            ir.functions[function as usize].name,
        );
        return Err(ReentryDecline::UnsupportedCallShape);
    }
    let source_receiver = ir.functions[function as usize]
        .dispatch_receiver
        .filter(|_| !ir.functions[function as usize].is_static);
    let receiver_offset = u32::from(source_receiver.is_some());
    let has_unplanned_shared_parameter =
        ir.shared_capture_parameters
            .iter()
            .any(|(&(owner, parameter), &element)| {
                owner == function
                    && receiver_offset
                        .checked_add(parameter)
                        .and_then(|slot| source_values.get(&slot))
                        != Some(&element)
            });
    if has_unplanned_shared_parameter || body_contains_unplanned_shared_cell(ir, body) {
        crate::trace_compiler!(
            "suspend",
            "state-machine function={function} name={:?} has unplanned shared-cell carrier: shared_parameters={:?} source_values={source_values:?}",
            ir.functions[function as usize].name,
            ir.shared_capture_parameters
                .iter()
                .filter(|((owner, _), _)| *owner == function)
                .collect::<Vec<_>>(),
        );
        // Every shared capture parameter must have the matching semantic element type in the
        // source value namespace. Its frame field is then emitted as the existing target-owned
        // shared-cell carrier.
        return Err(ReentryDecline::UnsupportedCallShape);
    }
    let receiver = source_receiver;
    let receiver_slots = u32::from(receiver.is_some());
    let completion_slot = receiver_slots + ir.functions[function as usize].params.len() as u32 - 1;
    let any = Ty::nullable(Ty::obj_name(wk::any()));
    let machine_result_ty = Ty::obj_args_name(type_name("kotlin/Result"), &[any]);
    let completion_result_ty = Ty::obj_args_name(type_name("kotlin/Result"), &[function_result]);
    let class_name = machine_name(ir, function)?;
    let type_arguments = type_parameters
        .iter()
        .map(IrTypeParameter::ty)
        .collect::<Vec<_>>();
    let machine_ty = if type_arguments.is_empty() {
        Ty::obj_name(class_name)
    } else {
        Ty::obj_args_name(class_name, &type_arguments)
    };
    let class = ir.add_class(IrClass::synthetic(class_name));
    {
        let machine = &mut ir.classes[class as usize];
        machine.is_compiler_generated = true;
        machine.is_local_class = true;
        machine.is_source_declared = true;
        machine.declaration_context = Some(crate::ir::IrClassDeclarationContext::Executable);
        machine.enclosure = Some(IrEnclosure::Function(function));
        machine.superclass = wk::any();
        machine.interfaces = vec![wk::continuation()].into();
        machine.supertypes = vec![Ty::obj_args_name(wk::continuation(), &[any])];
    }
    if !type_parameters.is_empty() {
        ir.insert_class_signature_name(
            class_name,
            IrGenericSig {
                type_params: type_parameters.clone(),
                params: Vec::new(),
                ret: None,
                supers: vec![Ty::obj_args_name(wk::continuation(), &[any])],
            },
        );
    }

    let frame = plan_frame_values(ir, function, body, class);
    let result_field = u32::try_from(ir.classes[class as usize].fields.len())
        .expect("a continuation frame has fewer than u32::MAX fields");
    ir.classes[class as usize]
        .fields
        .push(IrField::new("result".to_string(), machine_result_ty).with_is_private(false));
    let label_field = u32::try_from(ir.classes[class as usize].fields.len())
        .expect("a continuation frame has fewer than u32::MAX fields");
    ir.classes[class as usize]
        .fields
        .push(IrField::new("label".to_string(), Ty::Int).with_is_private(false));
    let constructor_value_count = receiver_slots + source_parameters.len() as u32 + 1;
    let completion_field = *frame.slots.get(&completion_slot).ok_or_else(|| {
        crate::trace_compiler!(
            "suspend",
            "state-machine function={function} name={:?} omitted completion value slot={completion_slot} from frame plan: slots={:?}",
            ir.functions[function as usize].name,
            frame.slots,
        );
        ReentryDecline::UnsupportedCallShape
    })?;
    let storage = ContinuationFields {
        class,
        machine_ty,
        label: label_field,
        result: result_field,
        runtime,
        suspend_functions: suspend_functions.clone(),
    };
    let next_value = max_value_index(ir).saturating_add(1);
    let labels = match split_at_suspensions(ir, body, suspend_functions, &storage, next_value) {
        Ok(labels) => labels,
        Err(error) => {
            crate::trace_compiler!(
                "suspend",
                "state-machine function={function} name={:?} splitter declined body={body} error={error:?} body_node={:?}",
                ir.functions[function as usize].name,
                ir.expr(body),
            );
            return Err(error);
        }
    };
    if labels == 0 {
        crate::trace_compiler!(
            "suspend",
            "state-machine function={function} name={:?} splitter accepted zero suspension labels",
            ir.functions[function as usize].name,
        );
        return Err(ReentryDecline::UnsupportedCallShape);
    }
    realize_frame_values(ir, &frame, class, machine_ty);

    let run = ir.add_fun(IrFunction {
        name: "invokeSuspend".to_string(),
        params: Vec::new(),
        ret: any,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(class_name),
        param_checks: Vec::new(),
    });
    ir.synthetic_methods.insert(run);
    ir.note_class_method(class, run);

    let resume = build_resume_with(
        ir,
        ResumeContract {
            class,
            runtime,
            machine_result_ty,
            completion_result_ty,
            function_result,
            any,
            completion_field,
            result_field,
        },
    )?;
    ir.synthetic_methods.insert(resume);
    ir.note_class_method(class, resume);
    let context = build_context_getter(ir, class, runtime, completion_field)?;
    ir.synthetic_methods.insert(context);
    ir.note_class_method(class, context);
    ir.classes[class as usize].methods = vec![run, resume, context];
    publish_resume_with(
        ir,
        class,
        resume,
        runtime.resume_with,
        runtime.resume_with_declared_parameter,
        runtime.resume_with_declared_result,
        machine_result_ty,
    );
    publish_context_override(ir, class, context, runtime);

    let constructor_args = (0..constructor_value_count)
        .map(|slot| {
            let value = ir.add_expr(IrExpr::GetValue(slot));
            let value_ty = if slot == completion_slot {
                ir.functions[function as usize].params[slot as usize - receiver_slots as usize]
            } else if receiver.is_some() && slot == 0 {
                Ty::obj_name(receiver.expect("receiver slot has a receiver"))
            } else {
                source_parameters[(slot - receiver_slots) as usize]
            };
            ir.set_logical_type(value, value_ty);
            value
        })
        .collect::<Vec<_>>();
    let mut ctor_args = Vec::with_capacity(constructor_args.len());
    for slot in 0..constructor_args.len() as u32 {
        let frame_slot = slot;
        let field_index = *frame
            .slots
            .get(&frame_slot)
            .ok_or(ReentryDecline::UnsupportedCallShape)?;
        let ty = if slot == completion_slot {
            ir.functions[function as usize].params[slot as usize - receiver_slots as usize]
        } else if receiver.is_some() && slot == 0 {
            Ty::obj_name(receiver.expect("receiver slot has a receiver"))
        } else {
            source_parameters[(slot - receiver_slots) as usize]
        };
        ctor_args.push(IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty,
            declared_ty: Some(ty),
            is_field: true,
            field_index: Some(field_index),
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            // These are ordinary generated primary-constructor value parameters. They are not
            // closure-prefix roles: the shared constructor emitter stores their explicit
            // field_index values using the same field-store plan as source `val` parameters.
            provenance: IrCtorParameterProvenance::Value,
            capture_identity: None,
        });
    }
    {
        let machine = &mut ir.classes[class as usize];
        machine.constructor_prefix_count = 0;
        machine.ctor_param_count = constructor_value_count;
        machine.ctor_args = ctor_args;
    }
    let construct = ir.add_expr(IrExpr::New {
        internal: class_name,
        args: constructor_args,
        ctor_params: None,
        declared_params: Some(
            ir.classes[class as usize]
                .ctor_args
                .iter()
                .map(|argument| argument.ty)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        ctor_desc: None,
        external_target: None,
        defaults: Box::default(),
        default_prefix_count: 0,
    });
    ir.set_logical_type(construct, machine_ty);
    let invoke = ir.add_expr(IrExpr::MethodCall {
        class,
        index: 0,
        receiver: construct,
        args: Vec::new(),
    });
    ir.set_logical_type(invoke, any);
    let returned = ir.add_expr(IrExpr::Return(Some(invoke)));
    let wrapper = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    ir.functions[function as usize].body = Some(wrapper);
    let source = ir
        .source_debug
        .current()
        .ok_or(ReentryDecline::UnsupportedCallShape)?;
    let package = type_name(&ir.package.as_deref().unwrap_or("").replace('.', "/"));
    ir.local_class_name_provenance.insert(
        class,
        IrLocalClassNameProvenance {
            source: IrModuleSource { source, package },
            lexical_owner: None,
            segments: vec![format!("Continuation${}${function}", source.raw())].into(),
            ordinal: None,
            parents: Box::default(),
        },
    );
    Ok(())
}

fn build_context_getter(
    ir: &mut IrFile,
    class: ClassId,
    runtime: crate::ir::IrCoroutineRuntime,
    completion_field: u32,
) -> Result<u32, ReentryDecline> {
    let owner = ir.classes[class as usize].fq_name_id();
    let machine = ir.add_expr(IrExpr::GetValue(0));
    ir.set_logical_type(machine, Ty::obj_name(owner));
    let completion = ir.add_expr(IrExpr::GetField {
        receiver: machine,
        class,
        index: completion_field,
    });
    let context = ir.add_expr(IrExpr::Checked(Box::new(
        crate::ir::IrCheckedOperation::ExternalPropertyRead {
            target: runtime.context,
            dispatch: crate::ir::IrPropertyDispatch::Ordinary,
            receiver: Some(completion),
            arguments: Vec::new(),
            parameters: Vec::new(),
            result: runtime.context_ty,
            source_receiver: Some(runtime.context_receiver_ty),
        },
    )));
    let function = ir.add_fun(IrFunction {
        name: "getContext".to_string(),
        params: Vec::new(),
        ret: runtime.context_ty,
        body: Some(context),
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: Vec::new(),
    });
    Ok(function)
}

fn publish_context_override(
    ir: &mut IrFile,
    class: ClassId,
    function: u32,
    runtime: crate::ir::IrCoroutineRuntime,
) {
    let owner = ir.classes[class as usize].fq_name_id();
    let target = crate::fir::ResolvedPropertyOverrideTarget::External(runtime.context_getter);
    ir.property_overrides
        .entry(owner)
        .or_default()
        .push(IrPropertyOverride {
            implementation: target,
            implementation_getter: Some(function),
            implementation_setter: None,
            implementation_owner: owner,
            overridden: target,
            overridden_owner: wk::continuation(),
            overridden_is_interface: true,
            name: "context".to_string(),
            declared_type: runtime.context_ty,
            applied_type: runtime.context_ty,
            implementation_type: runtime.context_ty,
            declared_receiver: None,
            implementation_receiver: None,
            overridden_setter_value: None,
            implementation_mutable: false,
            has_kotlin_superclass_override: false,
            depth: 1,
        });
}

fn body_contains_unplanned_shared_cell(ir: &IrFile, body: ExprId) -> bool {
    let mut pending = vec![body];
    let mut seen = HashSet::default();
    let mut planned = HashSet::default();
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        if let IrExpr::Variable {
            init: Some(initializer),
            ..
        } = ir.expr(expression)
        {
            if matches!(ir.expr(*initializer), IrExpr::RefNew { .. }) {
                planned.insert(*initializer);
            }
        }
        match ir.expr(expression) {
            IrExpr::Lambda { captures, .. } => pending.extend(captures.iter().copied()),
            _ => {
                crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
            }
        }
    }
    seen.into_iter().any(|expression| {
        matches!(ir.expr(expression), IrExpr::RefNew { .. }) && !planned.contains(&expression)
    })
}

/// Type parameters a generated machine must carry to keep its spilled fields exact. A local
/// continuation class is its own generic declaration, so parameters captured from the source
/// callable or enclosing class become parameters of this generated class while keeping their
/// original semantic names and bounds.
fn machine_type_parameters(ir: &IrFile, function: u32) -> Vec<IrTypeParameter> {
    let mut candidates = Vec::new();
    if let Some(owner) = ir.functions[function as usize].dispatch_receiver {
        if let Some(signature) = ir.class_signature_name(owner) {
            candidates.extend(signature.type_params.iter().cloned());
        }
    }
    if let Some(signature) = ir.signatures.get(&function) {
        candidates.extend(signature.type_params.iter().cloned());
    }
    candidates.extend(
        ir.recorded_lambda_type_parameters(function)
            .into_iter()
            .flatten()
            .cloned(),
    );

    let mut names = HashSet::default();
    candidates
        .into_iter()
        .filter_map(|mut parameter| {
            if !names.insert(parameter.semantic_name.clone()) {
                return None;
            }
            parameter.captured_from_enclosing = false;
            Some(parameter)
        })
        .collect()
}

fn machine_name(ir: &IrFile, function: u32) -> Result<TypeName, ReentryDecline> {
    let source = ir
        .source_debug
        .current()
        .ok_or(ReentryDecline::UnsupportedCallShape)?;
    let package = ir.package.as_deref().unwrap_or("").replace('.', "/");
    let segment = format!("$Continuation${}${function}", source.raw());
    let classifier = if package.is_empty() {
        segment
    } else {
        format!("{package}/{segment}")
    };
    Ok(type_name(&classifier))
}

fn build_resume_with(ir: &mut IrFile, contract: ResumeContract) -> Result<u32, ReentryDecline> {
    let ResumeContract {
        class,
        runtime,
        machine_result_ty,
        completion_result_ty,
        function_result,
        any,
        completion_field,
        result_field,
    } = contract;
    let owner = ir.classes[class as usize].fq_name_id();
    let completion_ty = ir.classes[class as usize].fields[completion_field as usize].ty;
    let exception_index = 2;
    let returned_index = 3;
    let result = ir.add_expr(IrExpr::GetValue(1));
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    ir.set_logical_type(receiver, Ty::obj_name(owner));
    let store = ir.add_expr(IrExpr::SetField {
        receiver,
        class,
        index: result_field,
        value: result,
    });
    let value = ir.add_expr(IrExpr::Variable {
        index: returned_index,
        ty: any,
        init: None,
        named: false,
    });
    let self_value = ir.add_expr(IrExpr::GetValue(0));
    ir.set_logical_type(self_value, Ty::obj_name(owner));
    let run_call = ir.add_expr(IrExpr::MethodCall {
        class,
        index: 0,
        receiver: self_value,
        args: Vec::new(),
    });
    ir.set_logical_type(run_call, any);
    let save_return = ir.add_expr(IrExpr::SetValue {
        var: returned_index,
        value: run_call,
    });
    let attempt = ir.add_expr(IrExpr::Block {
        stmts: vec![value, save_return],
        value: None,
    });
    let exception = ir.add_expr(IrExpr::GetValue(exception_index));
    let failed = failure_result(
        ir,
        runtime.result_failure,
        runtime.result_companion,
        completion_result_ty,
        function_result,
        exception,
    )?;
    let notify_failure = notify_completion(
        ir,
        class,
        runtime.resume_with,
        completion_field,
        completion_ty,
        completion_result_ty,
        failed,
    )?;
    let return_none = ir.add_expr(IrExpr::Return(None));
    let caught = ir.add_expr(IrExpr::Block {
        stmts: vec![notify_failure, return_none],
        value: None,
    });
    let attempt = ir.add_expr(IrExpr::Try {
        body: attempt,
        catches: vec![IrCatch::generated(
            exception_index,
            type_name("kotlin/Throwable"),
            caught,
        )],
        finally: None,
        result: Ty::Unit,
    });
    let marker = ir.add_expr(IrExpr::Checked(Box::new(
        crate::ir::IrCheckedOperation::ExternalPropertyRead {
            target: runtime.suspended,
            dispatch: crate::ir::IrPropertyDispatch::Ordinary,
            receiver: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            result: any,
            source_receiver: None,
        },
    )));
    let value = ir.add_expr(IrExpr::GetValue(returned_index));
    let pending = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::RefEq,
        lhs: value,
        rhs: marker,
    });
    let pending_body = ir.add_expr(IrExpr::Block {
        stmts: Vec::new(),
        value: None,
    });
    let value = ir.add_expr(IrExpr::GetValue(returned_index));
    let success_value = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: value,
        type_operand: function_result,
    });
    ir.set_logical_type(success_value, function_result);
    let companion = runtime.result_companion;
    let companion_value = ir.add_expr(IrExpr::SingletonValue {
        classifier: companion,
    });
    ir.set_logical_type(companion_value, Ty::obj_name(companion));
    let success = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::External {
            target: runtime.result_success,
            default_provider: None,
            params: vec![function_result],
            ret: completion_result_ty,
            substitutions: vec![external_type_substitution(
                runtime.result_success,
                function_result,
            )],
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: Some(companion_value),
        args: vec![success_value],
    });
    ir.call_declared_ret.insert(success, completion_result_ty);
    ir.external_singleton_dispatches.insert(success, companion);
    let notified = notify_completion(
        ir,
        class,
        runtime.resume_with,
        completion_field,
        completion_ty,
        completion_result_ty,
        success,
    )?;
    let success_body = ir.add_expr(IrExpr::Block {
        stmts: vec![notified],
        value: None,
    });
    let not_pending = ir.add_expr(IrExpr::When {
        branches: vec![(Some(pending), pending_body), (None, success_body)],
    });
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![store, attempt, not_pending],
        value: None,
    });
    let resume = ir.add_fun(IrFunction {
        name: "resumeWith".to_string(),
        params: vec![machine_result_ty],
        ret: Ty::Unit,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(owner),
        param_checks: vec![None],
    });
    ir.fn_params.insert(
        resume,
        crate::ir::FnParamInfo::identities(vec![IrParameterIdentity::generated(
            IrGeneratedParameterRole::ContinuationResult,
            None,
        )]),
    );
    Ok(resume)
}

fn failure_result(
    ir: &mut IrFile,
    failure: crate::fir::ExternalCallableId,
    companion: TypeName,
    result_ty: Ty,
    function_result: Ty,
    exception: ExprId,
) -> Result<ExprId, ReentryDecline> {
    let companion_value = ir.add_expr(IrExpr::SingletonValue {
        classifier: companion,
    });
    ir.set_logical_type(companion_value, Ty::obj_name(companion));
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::External {
            target: failure,
            default_provider: None,
            params: vec![Ty::obj_name(type_name("kotlin/Throwable"))],
            ret: result_ty,
            substitutions: vec![external_type_substitution(failure, function_result)],
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: Some(companion_value),
        args: vec![exception],
    });
    ir.call_declared_ret.insert(call, result_ty);
    ir.external_singleton_dispatches.insert(call, companion);
    Ok(call)
}

fn notify_completion(
    ir: &mut IrFile,
    class: ClassId,
    resume_with: crate::fir::ExternalCallableId,
    completion_field: u32,
    completion_ty: Ty,
    result_ty: Ty,
    result: ExprId,
) -> Result<ExprId, ReentryDecline> {
    let machine = ir.add_expr(IrExpr::GetValue(0));
    ir.set_logical_type(
        machine,
        Ty::obj_name(ir.classes[class as usize].fq_name_id()),
    );
    let completion = ir.add_expr(IrExpr::GetField {
        receiver: machine,
        class,
        index: completion_field,
    });
    ir.set_logical_type(completion, completion_ty);
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(Callee::External {
            target: resume_with,
            default_provider: None,
            params: vec![result_ty],
            ret: Ty::Unit,
            substitutions: Vec::new(),
            defaults: Vec::new(),
            extension_receiver_parameter: None,
        }),
        dispatch_receiver: Some(completion),
        args: vec![result],
    });
    ir.call_declared_ret.insert(call, Ty::Unit);
    Ok(call)
}

fn publish_resume_with(
    ir: &mut IrFile,
    class: ClassId,
    function: u32,
    identity: crate::fir::ExternalCallableId,
    declared_parameter: Ty,
    declared_result: Ty,
    result_ty: Ty,
) {
    let owner = ir.classes[class as usize].fq_name_id();
    let target = crate::fir::ResolvedFunctionOverrideTarget::External(identity);
    ir.function_overrides
        .entry(owner)
        .or_default()
        .push(IrFunctionOverride {
            implementation: target,
            implementation_function: Some(function),
            implementation_owner: owner,
            overridden: target,
            overridden_owner: wk::continuation(),
            overridden_semantic_role: None,
            collection_barrier: None,
            overridden_is_interface: true,
            name: "resumeWith".to_string(),
            declared_parameters: vec![declared_parameter],
            declared_result,
            applied_parameters: vec![result_ty],
            applied_result: Ty::Unit,
            implementation_parameters: vec![result_ty],
            implementation_parameter_identities: vec![
                crate::fir::ResolvedParameterIdentity::Unnamed { ordinal: 0 },
            ],
            overridden_parameter_identities: vec![crate::fir::ResolvedParameterIdentity::Unnamed {
                ordinal: 0,
            }],
            implementation_result: Ty::Unit,
            suspend: false,
            has_kotlin_superclass_override: false,
            depth: 1,
        });
}
