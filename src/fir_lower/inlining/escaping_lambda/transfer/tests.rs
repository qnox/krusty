use super::*;
use crate::ir::{
    DeclarationAnnotations, FnParamInfo, IrCapturedDeclaration, IrCapturingCallable, IrFunction,
    IrParameterIdentity, IrParameterRole, IrValueCapture, IrVarargParameter,
};
use crate::types::{InlineExpansionMode, InlineParameterModifier};

const LOCAL_CAPTURE: IrValueCapture = IrValueCapture {
    declaration: IrCapturedDeclaration::Variable,
    capturer: IrCapturingCallable::Lambda,
};

fn captured(name: &str, ordinal: u32) -> IrParameterIdentity {
    IrParameterIdentity::captured_value(Some(name.to_string()), ordinal, LOCAL_CAPTURE)
}

#[test]
fn transferred_parameter_checks_preserve_sparse_sidecars() {
    let mut ir = crate::ir::IrFile::default();
    let first = ir.add_expr(IrExpr::UnitInstance);
    let second = ir.add_expr(IrExpr::UnitInstance);
    let implementation = ir.add_fun(IrFunction {
        name: "captured".to_string(),
        params: vec![Ty::String, Ty::String],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![Some(crate::ir::IrParameterCheck::NonNull), None],
    });
    let lambda = ir.add_expr(IrExpr::Lambda {
        impl_fn: implementation,
        arity: 0,
        captures: vec![first, second],
        sam: None,
        inline_body: None,
    });
    let captures = [InlineCapture {
        parameter: 0,
        lambda,
    }];

    assert_eq!(
        replaced_parameter_checks(&ir, &[], &captures, 1),
        Some(vec![Some(crate::ir::IrParameterCheck::NonNull), None])
    );

    ir.functions[implementation as usize].param_checks.clear();
    assert_eq!(
        replaced_parameter_checks(
            &ir,
            &[Some(crate::ir::IrParameterCheck::NonNull), None],
            &[InlineCapture {
                parameter: 1,
                lambda,
            }],
            2,
        ),
        Some(vec![Some(crate::ir::IrParameterCheck::NonNull), None, None])
    );
}

#[test]
fn splicing_a_middle_capture_keeps_parameter_checks_parallel() {
    let mut ir = crate::ir::IrFile::default();
    let mut sources = crate::fir::SourceMap::default();
    let source = sources.insert("Main.kt");
    ir.source_debug = crate::ir::SourceDebug::from_map(&sources, source, &[20]);
    let actual_body = ir.add_expr(IrExpr::UnitInstance);
    let actual_implementation = ir.add_fun(IrFunction {
        name: "actual".to_string(),
        params: Vec::new(),
        ret: Ty::Unit,
        body: Some(actual_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: actual_implementation,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(actual_body),
    });
    let captured_read = ir.add_expr(IrExpr::GetValue(1));
    let body = ir.add_expr(IrExpr::InvokeFunction {
        func: captured_read,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::Unit,
    });
    let declaration_implementation = ir.add_fun(IrFunction {
        name: "declaration".to_string(),
        params: Vec::new(),
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.record_inline_copy_owner(
        body,
        declaration_implementation,
        None,
        crate::fir::SourceFileId::from_raw(7),
    );
    let invocation_provenance = ir
        .inline_copy_provenance(body)
        .expect("the declaration invocation is an exact inline copy")
        .to_vec();
    let mut shape = IrFunction {
        name: "escaping".to_string(),
        params: vec![Ty::String, Ty::fun(Vec::new(), Ty::Unit), Ty::String],
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![
            Some(crate::ir::IrParameterCheck::NonNull),
            None,
            Some(crate::ir::IrParameterCheck::NonNull),
        ],
    };

    assert_eq!(
        splice_inline_captures(
            &mut ir,
            body,
            &mut shape,
            &[InlineCapture {
                parameter: 1,
                lambda: actual,
            }],
            "inlineCall",
        ),
        Some(())
    );
    assert_eq!(shape.params, [Ty::String, Ty::String]);
    assert_eq!(
        shape.param_checks,
        [
            Some(crate::ir::IrParameterCheck::NonNull),
            Some(crate::ir::IrParameterCheck::NonNull),
        ]
    );
    let IrExpr::Block {
        stmts: outer_statements,
        value: Some(frame),
    } = ir.expr(body)
    else {
        panic!("the declaration invocation remains the outer frame")
    };
    assert!(outer_statements.is_empty());
    let IrExpr::Block {
        stmts: frame_statements,
        value: Some(_),
    } = ir.expr(*frame)
    else {
        panic!("the supplied lambda owns a nested value frame")
    };
    assert!(frame_statements.iter().any(|&statement| matches!(
        ir.debug_local_provenance(statement),
        Some(crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
            implementation,
            ..
        }) if implementation == actual_implementation
    )));
    let frame_provenance = ir
        .inline_copy_provenance(*frame)
        .expect("the nested frame keeps both source identities");
    assert_eq!(frame_provenance.len(), invocation_provenance.len() + 1);
    assert_eq!(frame_provenance[0].function, actual_implementation);
    assert_eq!(&frame_provenance[1..], invocation_provenance.as_slice());
    assert_eq!(frame_provenance[1].function, declaration_implementation);
}

#[test]
fn captured_lambda_substitution_refreshes_the_retained_inline_body() {
    let mut ir = crate::ir::IrFile::default();
    let actual_body = ir.add_expr(IrExpr::UnitInstance);
    let actual_implementation = ir.add_fun(IrFunction {
        name: "actual".to_string(),
        params: Vec::new(),
        ret: Ty::Unit,
        body: Some(actual_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: actual_implementation,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(actual_body),
    });
    let implementation_read = ir.add_expr(IrExpr::GetValue(0));
    let implementation_body = ir.add_expr(IrExpr::InvokeFunction {
        func: implementation_read,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::Unit,
    });
    let implementation = ir.add_fun(IrFunction {
        name: "escaping".to_string(),
        params: vec![Ty::fun(Vec::new(), Ty::Unit)],
        ret: Ty::Unit,
        body: Some(implementation_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let retained_read = ir.add_expr(IrExpr::GetValue(0));
    let retained_inline_body = ir.add_expr(IrExpr::InvokeFunction {
        func: retained_read,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::Unit,
    });
    ir.set_logical_type(retained_inline_body, Ty::Unit);
    let copy = ir.add_expr(IrExpr::Lambda {
        impl_fn: implementation,
        arity: 0,
        captures: vec![actual],
        sam: None,
        inline_body: Some(retained_inline_body),
    });

    let plan = plan(
        &ir,
        &[(copy, copy)],
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([actual]),
        &HashSet::default(),
        &HashMap::default(),
    )
    .expect("the exact capture-free lambda invocation is transferable");
    let suppressions = apply(
        &mut ir,
        plan,
        &HashMap::default(),
        &HashMap::default(),
        crate::fir::DeclarationId::from_raw(0),
        None,
        false,
        "caller",
        implementation,
        None,
        crate::fir::SourceFileId::from_raw(0),
        crate::fir::CallableId::from_raw(0),
        "inlineCall",
    );

    assert_eq!(suppressions, [actual_implementation]);
    let IrExpr::Lambda {
        impl_fn: specialized,
        captures,
        inline_body: Some(specialized_inline_body),
        ..
    } = ir.expr(copy)
    else {
        panic!("the escaping copy remains an inlineable lambda")
    };
    assert!(captures.is_empty());
    assert_ne!(*specialized, implementation);
    assert!(ir.specialized_functions[specialized].consumed_inline_capture);
    let specialized_body = ir.functions[*specialized as usize]
        .body
        .expect("the specialized implementation retains a body");
    assert_eq!(*specialized_inline_body, retained_inline_body);
    assert_ne!(*specialized_inline_body, specialized_body);
    assert_eq!(ir.logical_type(*specialized_inline_body), Some(Ty::Unit));
    let IrExpr::Block {
        stmts: outer_statements,
        value: Some(frame),
    } = ir.expr(*specialized_inline_body)
    else {
        panic!("the retained body owns the specialized outer frame")
    };
    assert!(outer_statements.is_empty());
    assert!(matches!(
        ir.expr(*frame),
        IrExpr::Block { stmts, value: Some(_) }
            if stmts.iter().any(|&statement| matches!(
                ir.debug_local_provenance(statement),
                Some(crate::ir::IrDebugLocalProvenance::LambdaFrameMarker {
                    implementation,
                    ..
                }) if implementation == actual_implementation
            ))
    ));
}

#[test]
fn removing_a_middle_inline_capture_keeps_every_parameter_fact_aligned() {
    let mut ir = crate::ir::IrFile::default();
    let source = ir.add_fun(IrFunction {
        name: "actual".to_string(),
        params: Vec::new(),
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.fn_params
        .insert(source, FnParamInfo::identities(Vec::new()));
    ir.lambda_own_params_from.insert(source, 0);
    let actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: source,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: None,
    });
    // The function shape has already dropped the captured inline parameter when its copied
    // sidecars are rewritten.
    let function = ir.add_fun(IrFunction {
        name: "copy".to_string(),
        params: vec![Ty::Int, Ty::String],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let first_default = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
    let removed_default = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(2)));
    let last_default = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(3)));
    ir.fn_params.insert(
        function,
        FnParamInfo {
            identities: vec![
                captured("first", 0),
                captured("removed", 1),
                captured("last", 2),
            ],
            defaults: Some(vec![
                Some(first_default),
                Some(removed_default),
                Some(last_default),
            ]),
            inline_modifiers: vec![
                InlineParameterModifier::None,
                InlineParameterModifier::Noinline,
                InlineParameterModifier::Crossinline,
            ],
            expansion_modes: vec![
                InlineExpansionMode::Receiver,
                InlineExpansionMode::Materialize,
                InlineExpansionMode::Splice,
            ],
            ..FnParamInfo::default()
        },
    );
    ir.fn_param_declared_nullable
        .insert(function, vec![true, false, true]);
    ir.fn_param_annotations.insert(
        function,
        vec![
            DeclarationAnnotations::default().with_suppressed_source_application(true),
            DeclarationAnnotations::default(),
            DeclarationAnnotations::default(),
        ],
    );
    ir.fn_param_no_infer
        .insert(function, vec![false, true, true]);
    ir.lambda_own_params_from.insert(function, 3);
    ir.fn_varargs.insert(
        function,
        IrVarargParameter {
            index: 2,
            is_last: true,
        },
    );
    ir.shared_capture_parameters.insert((function, 0), Ty::Int);
    ir.shared_capture_parameters
        .insert((function, 1), Ty::Boolean);
    ir.shared_capture_parameters
        .insert((function, 2), Ty::String);

    transfer_inlined_capture_facts(
        &mut ir,
        function,
        &[InlineCapture {
            parameter: 1,
            lambda: actual,
        }],
    );

    let info = &ir.fn_params[&function];
    assert_eq!(
        info.identities
            .iter()
            .map(|identity| (identity.source_name.as_deref(), identity.role))
            .collect::<Vec<_>>(),
        [
            (
                Some("first"),
                IrParameterRole::CapturedValue {
                    ordinal: 0,
                    capture: LOCAL_CAPTURE,
                },
            ),
            (
                Some("last"),
                IrParameterRole::CapturedValue {
                    ordinal: 1,
                    capture: LOCAL_CAPTURE,
                },
            ),
        ]
    );
    assert_eq!(
        info.defaults.as_deref(),
        Some([Some(first_default), Some(last_default)].as_slice())
    );
    assert_eq!(
        info.inline_modifiers,
        [
            InlineParameterModifier::None,
            InlineParameterModifier::Crossinline
        ]
    );
    assert_eq!(
        info.expansion_modes,
        [InlineExpansionMode::Receiver, InlineExpansionMode::Splice]
    );
    assert_eq!(ir.fn_param_declared_nullable[&function], [true, true]);
    assert_eq!(ir.fn_param_annotations[&function].len(), 2);
    assert!(ir.fn_param_annotations[&function][0].has_suppressed_source_application());
    assert!(!ir.fn_param_annotations[&function][1].declares_annotations());
    assert_eq!(ir.fn_param_no_infer[&function], [false, true]);
    assert_eq!(ir.lambda_own_params_from[&function], 2);
    assert_eq!(ir.fn_varargs[&function].index, 1);
    assert_eq!(ir.shared_capture_parameters[&(function, 0)], Ty::Int);
    assert_eq!(ir.shared_capture_parameters[&(function, 1)], Ty::String);
    assert!(!ir.shared_capture_parameters.contains_key(&(function, 2)));
}

#[test]
fn failed_later_root_preflight_leaves_no_specialized_artifacts() {
    let mut ir = crate::ir::IrFile::default();
    let actual_body = ir.add_expr(IrExpr::UnitInstance);
    let actual_implementation = ir.add_fun(IrFunction {
        name: "actual".to_string(),
        params: vec![Ty::Int],
        ret: Ty::Unit,
        body: Some(actual_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: actual_implementation,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(actual_body),
    });
    let captured_read = ir.add_expr(IrExpr::GetValue(0));
    let invocation = ir.add_expr(IrExpr::InvokeFunction {
        func: captured_read,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::Unit,
    });
    let escaping_implementation = ir.add_fun(IrFunction {
        name: "escaping".to_string(),
        params: vec![Ty::fun(Vec::new(), Ty::Unit)],
        ret: Ty::Unit,
        body: Some(invocation),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let copy = ir.add_expr(IrExpr::Lambda {
        impl_fn: escaping_implementation,
        arity: 0,
        captures: vec![actual],
        sam: None,
        inline_body: None,
    });
    let before = (
        ir.exprs.len(),
        ir.functions.len(),
        ir.classes.len(),
        ir.specialized_functions.len(),
        ir.specialized_expansion_order.len(),
    );

    assert!(plan(
        &ir,
        &[(copy, copy)],
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([actual]),
        &HashSet::default(),
        &HashMap::default(),
    )
    .is_none());
    assert_eq!(
        (
            ir.exprs.len(),
            ir.functions.len(),
            ir.classes.len(),
            ir.specialized_functions.len(),
            ir.specialized_expansion_order.len(),
        ),
        before
    );
    assert_eq!(
        ir.functions[actual_implementation as usize].body,
        Some(actual_body)
    );
    assert_eq!(
        ir.functions[escaping_implementation as usize].body,
        Some(invocation)
    );
}

#[test]
fn directly_spliced_lambda_is_not_planned_as_escaping() {
    let mut ir = crate::ir::IrFile::default();
    let actual_body = ir.add_expr(IrExpr::UnitInstance);
    let actual_implementation = ir.add_fun(IrFunction {
        name: "actual".to_string(),
        params: Vec::new(),
        ret: Ty::Unit,
        body: Some(actual_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: actual_implementation,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(actual_body),
    });
    let captured_read = ir.add_expr(IrExpr::GetValue(0));
    let invocation = ir.add_expr(IrExpr::InvokeFunction {
        func: captured_read,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::Unit,
    });
    let implementation = ir.add_fun(IrFunction {
        name: "direct".to_string(),
        params: vec![Ty::fun(Vec::new(), Ty::Unit)],
        ret: Ty::Unit,
        body: Some(invocation),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let direct = ir.add_expr(IrExpr::Lambda {
        impl_fn: implementation,
        arity: 0,
        captures: vec![actual],
        sam: None,
        inline_body: Some(invocation),
    });

    let plan = plan(
        &ir,
        &[(direct, direct)],
        &HashSet::default(),
        &HashSet::from_iter([direct]),
        &HashSet::from_iter([actual]),
        &HashSet::default(),
        &HashMap::default(),
    )
    .expect("a directly spliced value does not need an escaping implementation");

    assert!(plan.roots.is_empty());
    assert!(plan.functions.is_empty());
}

#[test]
fn caller_captures_transfer_into_the_specialized_declaration_closure() {
    let mut ir = crate::ir::IrFile::default();
    let actual_body = ir.add_expr(IrExpr::UnitInstance);
    let actual_implementation = ir.add_fun(IrFunction {
        name: "actual".to_string(),
        params: vec![Ty::Int, Ty::String],
        ret: Ty::Unit,
        body: Some(actual_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let mut actual_parameters = FnParamInfo::identities(vec![
        captured("local", 0),
        IrParameterIdentity::captured_receiver(0),
    ]);
    actual_parameters
        .captured_receivers
        .push(crate::ir::IrCapturedReceiver::Lambda(None));
    ir.fn_params
        .insert(actual_implementation, actual_parameters);
    ir.lambda_own_params_from.insert(actual_implementation, 2);
    ir.shared_capture_parameters
        .insert((actual_implementation, 0), Ty::Int);
    let actual_capture = ir.add_expr(IrExpr::GetValue(0));
    let actual_receiver = ir.add_expr(IrExpr::GetValue(1));
    let actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: actual_implementation,
        arity: 0,
        captures: vec![actual_capture, actual_receiver],
        sam: None,
        inline_body: Some(actual_body),
    });
    let captured_read = ir.add_expr(IrExpr::GetValue(0));
    let invocation = ir.add_expr(IrExpr::InvokeFunction {
        func: captured_read,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::Unit,
    });
    let escaping_implementation = ir.add_fun(IrFunction {
        name: "escaping".to_string(),
        params: vec![Ty::fun(Vec::new(), Ty::Unit)],
        ret: Ty::Unit,
        body: Some(invocation),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.fn_params.insert(
        escaping_implementation,
        FnParamInfo::identities(vec![captured("second", 0)]),
    );
    ir.lambda_own_params_from.insert(escaping_implementation, 1);
    let copy = ir.add_expr(IrExpr::Lambda {
        impl_fn: escaping_implementation,
        arity: 0,
        captures: vec![actual],
        sam: None,
        inline_body: None,
    });
    let before = (
        ir.exprs.len(),
        ir.functions.len(),
        ir.classes.len(),
        ir.specialized_functions.len(),
        ir.specialized_expansion_order.len(),
    );

    let transfer_plan = plan(
        &ir,
        &[(copy, copy)],
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([actual]),
        &HashSet::default(),
        &HashMap::default(),
    )
    .expect("a caller-capturing substitution transfers into the site-specific closure");
    assert_eq!(transfer_plan.roots.len(), 1);
    assert_eq!(transfer_plan.roots[0].inline_captures.len(), 1);
    assert_eq!(
        transfer_plan.functions,
        HashSet::from_iter([escaping_implementation])
    );
    assert_eq!(
        (
            ir.exprs.len(),
            ir.functions.len(),
            ir.classes.len(),
            ir.specialized_functions.len(),
            ir.specialized_expansion_order.len(),
        ),
        before
    );
    assert_eq!(
        ir.functions[actual_implementation as usize].body,
        Some(actual_body)
    );
    assert_eq!(
        ir.functions[escaping_implementation as usize].body,
        Some(invocation)
    );
    let owned_edges = HashSet::from_iter([(copy, actual)]);
    let captured_plan = plan(
        &ir,
        &[(copy, copy)],
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([actual]),
        &owned_edges,
        &HashMap::default(),
    )
    .expect("anonymous specialization owns the retained constructor capture");
    assert!(captured_plan.roots.is_empty());
    assert!(captured_plan.functions.is_empty());

    let suppressions = apply(
        &mut ir,
        transfer_plan,
        &HashMap::default(),
        &HashMap::default(),
        crate::fir::DeclarationId::from_raw(0),
        None,
        false,
        "caller",
        escaping_implementation,
        None,
        crate::fir::SourceFileId::from_raw(0),
        crate::fir::CallableId::from_raw(0),
        "inlineCall",
    );
    assert_eq!(suppressions, [actual_implementation]);
    let IrExpr::Lambda {
        impl_fn: specialized,
        captures,
        inline_body,
        ..
    } = ir.expr(copy)
    else {
        panic!("the site-specific outer closure remains a lambda")
    };
    assert_ne!(*specialized, escaping_implementation);
    assert_eq!(captures, &[actual_capture, actual_receiver]);
    assert_eq!(*inline_body, None);
    assert_eq!(
        ir.specialized_functions[specialized].source,
        escaping_implementation
    );
    assert_eq!(ir.specialized_expansion_order[specialized], copy);
    assert_eq!(
        ir.functions[*specialized as usize].params,
        [Ty::Int, Ty::String]
    );
    assert_eq!(ir.lambda_own_params_from[specialized], 2);
    assert_eq!(
        ir.function_parameter_identities(*specialized)
            .expect("the specialized closure keeps its transferred capture identity")[0]
            .source_name
            .as_deref(),
        Some("local")
    );
    assert!(ir
        .function_parameter_identities(*specialized)
        .expect("the specialized closure keeps transferred capture provenance")
        .iter()
        .all(|identity| identity.provenance == crate::ir::IrParameterProvenance::InlinedCapture));
    assert_eq!(
        ir.function_parameter_identities(*specialized)
            .expect("the specialized closure keeps its transferred receiver identity")[1]
            .role,
        IrParameterRole::CapturedReceiver { ordinal: 0 }
    );
    assert_eq!(
        ir.fn_params[specialized].captured_receivers,
        [crate::ir::IrCapturedReceiver::Lambda(None)]
    );
    assert_eq!(ir.shared_capture_parameters[&(*specialized, 0)], Ty::Int);
    let IrExpr::Lambda {
        impl_fn,
        captures,
        inline_body,
        ..
    } = ir.expr(actual)
    else {
        panic!("the caller-owned substituted value remains a lambda")
    };
    assert_eq!(*impl_fn, actual_implementation);
    assert_eq!(captures, &[actual_capture, actual_receiver]);
    assert_eq!(*inline_body, Some(actual_body));
}

#[test]
fn captured_substituted_lambda_stays_materialized_while_capture_free_copy_specializes() {
    let mut ir = crate::ir::IrFile::default();
    let anonymous = crate::types::type_name("sample/InlineAnonymous");
    let mut anonymous_class = crate::ir::IrClass::synthetic(anonymous);
    anonymous_class.is_anonymous_object = true;
    ir.add_class(anonymous_class);
    let body = ir.add_expr(IrExpr::UnitInstance);
    let implementation = ir.add_fun(IrFunction {
        name: "supplied".to_string(),
        params: vec![Ty::obj_name(anonymous)],
        ret: Ty::Unit,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let captured_value = ir.add_expr(IrExpr::GetValue(0));
    let supplied = ir.add_expr(IrExpr::Lambda {
        impl_fn: implementation,
        arity: 0,
        captures: vec![captured_value],
        sam: None,
        inline_body: Some(body),
    });

    let captured_substitution_plan = plan(
        &ir,
        &[(supplied, supplied)],
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([supplied]),
        &HashSet::default(),
        &HashMap::default(),
    )
    .expect("a caller-capturing substituted lambda remains materialized");

    assert!(captured_substitution_plan.roots.is_empty());
    assert!(captured_substitution_plan.functions.is_empty());
    assert_eq!(ir.functions[implementation as usize].body, Some(body));
    let IrExpr::Lambda {
        impl_fn,
        captures,
        inline_body,
        ..
    } = ir.expr(supplied)
    else {
        panic!("the substituted lambda remains a lambda")
    };
    assert_eq!(*impl_fn, implementation);
    assert_eq!(captures, &[captured_value]);
    assert_eq!(*inline_body, Some(body));

    let capture_free = ir.add_expr(IrExpr::Lambda {
        impl_fn: implementation,
        arity: 1,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(body),
    });
    let plan = plan(
        &ir,
        &[(capture_free, capture_free)],
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([capture_free]),
        &HashSet::default(),
        &HashMap::default(),
    )
    .expect("the capture-free substituted lambda remains transferable");
    assert_eq!(plan.roots.len(), 1);
    assert_eq!(plan.functions, HashSet::from_iter([implementation]));

    let suppressions = apply(
        &mut ir,
        plan,
        &HashMap::default(),
        &HashMap::default(),
        crate::fir::DeclarationId::from_raw(0),
        None,
        false,
        "caller",
        implementation,
        None,
        crate::fir::SourceFileId::from_raw(0),
        crate::fir::CallableId::from_raw(0),
        "inlineCall",
    );
    assert!(suppressions.is_empty());
    assert_eq!(ir.functions[implementation as usize].body, Some(body));
    let IrExpr::Lambda {
        impl_fn: specialized,
        ..
    } = ir.expr(capture_free)
    else {
        panic!("the capture-free substitution remains a lambda")
    };
    assert_ne!(*specialized, implementation);
    assert!(!ir.specialized_functions[specialized].consumed_inline_capture);
    assert!(ir.functions[*specialized as usize].body.is_some());
    assert!(!ir.inline_only_fns.contains(&implementation));
    assert!(!ir.inline_only_fns.contains(specialized));
}

#[test]
fn invalid_nested_specialization_invalidates_the_outer_plan() {
    let mut ir = crate::ir::IrFile::default();
    let anonymous = crate::types::type_name("sample/InlineAnonymous");
    let mut anonymous_class = crate::ir::IrClass::synthetic(anonymous);
    anonymous_class.is_anonymous_object = true;
    anonymous_class.ctor_args.push(crate::ir::IrCtorArg {
        name: None,
        context_kind: crate::types::ContextParameterKind::None,
        ty: Ty::fun(Vec::new(), Ty::Unit),
        declared_ty: None,
        is_field: true,
        field_index: Some(0),
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture: None,
        provenance: crate::ir::IrCtorParameterProvenance::Capture,
        capture_identity: None,
    });
    ir.add_class(anonymous_class);

    let nested_implementation = ir.add_fun(IrFunction {
        name: "nested".to_string(),
        params: vec![Ty::obj_name(anonymous)],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let nested = ir.add_expr(IrExpr::Lambda {
        impl_fn: nested_implementation,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: None,
    });
    let actual_body = ir.add_expr(IrExpr::UnitInstance);
    let actual_implementation = ir.add_fun(IrFunction {
        name: "actual".to_string(),
        params: vec![Ty::Int],
        ret: Ty::Unit,
        body: Some(actual_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let actual_capture = ir.add_expr(IrExpr::GetValue(0));
    let actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: actual_implementation,
        arity: 0,
        captures: vec![actual_capture],
        sam: None,
        inline_body: Some(actual_body),
    });
    let captured_read = ir.add_expr(IrExpr::GetValue(0));
    let invocation = ir.add_expr(IrExpr::InvokeFunction {
        func: captured_read,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::Unit,
    });
    let outer_body = ir.add_expr(IrExpr::Block {
        stmts: vec![nested],
        value: Some(invocation),
    });
    let outer_implementation = ir.add_fun(IrFunction {
        name: "outer".to_string(),
        params: vec![Ty::fun(Vec::new(), Ty::Unit)],
        ret: Ty::Unit,
        body: Some(outer_body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let copy = ir.add_expr(IrExpr::Lambda {
        impl_fn: outer_implementation,
        arity: 0,
        captures: vec![actual],
        sam: None,
        inline_body: Some(outer_body),
    });
    ir.set_logical_type(copy, Ty::obj_name(anonymous));
    let construction = ir.add_expr(IrExpr::New {
        internal: anonymous,
        args: vec![copy],
        ctor_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });

    assert!(plan(
        &ir,
        &[(copy, copy), (construction, construction)],
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([actual]),
        &HashSet::default(),
        &HashMap::default(),
    )
    .is_none());
    assert!(ir.specialized_functions.is_empty());
    assert_eq!(ir.functions[nested_implementation as usize].body, None);
    assert_eq!(
        ir.functions[outer_implementation as usize].body,
        Some(outer_body)
    );

    let owned_edges =
        super::super::super::escaping_anonymous::owned_substituted_lambda_capture_edges(
            &ir,
            [construction],
            &HashSet::from_iter([actual]),
        );
    assert_eq!(owned_edges, HashSet::from_iter([(copy, actual)]));
    let plan = plan(
        &ir,
        &[(copy, copy), (construction, construction)],
        &HashSet::default(),
        &HashSet::default(),
        &HashSet::from_iter([actual]),
        &owned_edges,
        &HashMap::default(),
    )
    .expect("anonymous specialization owns the nested constructor lambda");
    assert!(plan.roots.is_empty());
    assert!(plan.functions.is_empty());
}

#[test]
fn duplicate_function_requests_require_identical_capture_provenance() {
    let mut ir = crate::ir::IrFile::default();
    let unit = ir.add_expr(IrExpr::UnitInstance);
    let actual_implementation = ir.add_fun(IrFunction {
        name: "actual".to_string(),
        params: Vec::new(),
        ret: Ty::Unit,
        body: Some(unit),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let first_actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: actual_implementation,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(unit),
    });
    let second_actual = ir.add_expr(IrExpr::Lambda {
        impl_fn: actual_implementation,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(unit),
    });
    let captured_read = ir.add_expr(IrExpr::GetValue(0));
    let invocation = ir.add_expr(IrExpr::InvokeFunction {
        func: captured_read,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::Unit,
    });
    let escaping = ir.add_fun(IrFunction {
        name: "escaping".to_string(),
        params: vec![Ty::fun(Vec::new(), Ty::Unit)],
        ret: Ty::Unit,
        body: Some(invocation),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    let first_copy = ir.add_expr(IrExpr::Lambda {
        impl_fn: escaping,
        arity: 0,
        captures: vec![first_actual],
        sam: None,
        inline_body: None,
    });
    let same_copy = ir.add_expr(IrExpr::Lambda {
        impl_fn: escaping,
        arity: 0,
        captures: vec![first_actual],
        sam: None,
        inline_body: None,
    });
    let different_copy = ir.add_expr(IrExpr::Lambda {
        impl_fn: escaping,
        arity: 0,
        captures: vec![second_actual],
        sam: None,
        inline_body: None,
    });
    let substituted = HashSet::from_iter([first_actual, second_actual]);

    let exact_request = [InlineCapture {
        parameter: 0,
        lambda: first_actual,
    }];
    let mut decisions = HashMap::default();
    let mut planned = HashSet::default();
    let mut requests = HashMap::default();
    let mut visiting = HashSet::default();
    assert_eq!(
        plan_function(
            &ir,
            escaping,
            first_copy,
            None,
            &HashMap::default(),
            &exact_request,
            &mut decisions,
            &mut planned,
            &mut requests,
            &mut visiting,
        ),
        FunctionPlan::Planned
    );
    assert_eq!(
        plan_function(
            &ir,
            escaping,
            first_copy,
            None,
            &HashMap::default(),
            &exact_request,
            &mut decisions,
            &mut planned,
            &mut requests,
            &mut visiting,
        ),
        FunctionPlan::Planned
    );
    assert!(plan(
        &ir,
        &[(first_copy, first_copy), (same_copy, same_copy)],
        &HashSet::default(),
        &HashSet::default(),
        &substituted,
        &HashSet::default(),
        &HashMap::default(),
    )
    .is_none());
    assert!(plan(
        &ir,
        &[(first_copy, first_copy), (different_copy, different_copy)],
        &HashSet::default(),
        &HashSet::default(),
        &substituted,
        &HashSet::default(),
        &HashMap::default(),
    )
    .is_none());
    assert!(ir.specialized_functions.is_empty());
}
