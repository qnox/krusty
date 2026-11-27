use super::*;

#[test]
fn anonymous_owned_edges_select_exact_capture_free_and_capture_owning_substitutions() {
    let mut ir = crate::ir::IrFile::default();
    let body = ir.add_expr(IrExpr::UnitInstance);
    let caller_value = ir.add_expr(IrExpr::GetValue(0));
    let captured = ir.add_expr(IrExpr::Lambda {
        impl_fn: 0,
        arity: 0,
        captures: vec![caller_value],
        sam: None,
        inline_body: Some(body),
    });
    let capture_free = ir.add_expr(IrExpr::Lambda {
        impl_fn: 1,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(body),
    });
    let unrelated = ir.add_expr(IrExpr::Lambda {
        impl_fn: 2,
        arity: 0,
        captures: Vec::new(),
        sam: None,
        inline_body: Some(body),
    });
    let constructor_lambda = ir.add_expr(IrExpr::Lambda {
        impl_fn: 3,
        arity: 0,
        captures: vec![captured, capture_free, unrelated],
        sam: None,
        inline_body: Some(body),
    });

    let name = crate::types::type_name("sample/InlineAnonymous");
    let mut class = crate::ir::IrClass::synthetic(name);
    class.is_anonymous_object = true;
    class.ctor_args.push(crate::ir::IrCtorArg {
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
    ir.add_class(class);
    let construction = ir.add_expr(IrExpr::New {
        internal: name,
        args: vec![constructor_lambda],
        ctor_params: None,
        declared_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });

    let owned_edges = owned_substituted_lambda_capture_edges(
        &ir,
        [construction],
        &HashSet::from_iter([captured, capture_free]),
    );
    assert_eq!(
        owned_edges,
        HashSet::from_iter([
            (constructor_lambda, captured),
            (constructor_lambda, capture_free),
        ])
    );
    assert!(!owned_edges.contains(&(constructor_lambda, unrelated)));
}

#[test]
fn specialized_transport_removal_does_not_retag_original_prefix_debug_identity() {
    use crate::ir::{
        IrCapturedDeclaration, IrCapturingCallable, IrParameterIdentity, IrValueCapture,
    };
    let mut ir = crate::ir::IrFile::default();
    let owner = crate::fir::DeclarationId::from_raw(11);
    let enclosing = crate::types::type_name("Outer");
    let mut original = crate::ir::IrClass::synthetic(crate::types::type_name("Original"));
    original
        .fields
        .push(crate::ir::IrField::new("value".into(), Ty::Int));
    original.constructor_prefix_count = 2;
    original.ctor_param_count = 1;
    let argument = |receiver: Option<crate::ir::IrCapturedReceiver>, field_index: Option<u32>| {
        crate::ir::IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty: Ty::obj_name(enclosing),
            declared_ty: None,
            is_field: field_index.is_some(),
            field_index,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: Some(crate::ir::IrConstructorCapture {
                source_name: "value".into(),
                receiver,
                crossinline_parameter: None,
                inlined: false,
            }),
            provenance: crate::ir::IrCtorParameterProvenance::Capture,
            capture_identity: None,
        }
    };
    original.ctor_args.push(argument(None, Some(0)));
    original.ctor_args.push(argument(
        Some(crate::ir::IrCapturedReceiver::Enclosing {
            classifier: enclosing,
            owner: None,
        }),
        None,
    ));
    let original = ir.add_class(original);
    ir.checked_classifier_classes.insert(owner, original);
    let mut copy = ir.classes[original as usize].clone();
    copy.fq_name = crate::types::type_name("Specialized");
    let copy_name = copy.fq_name;
    let copy = ir.add_class(copy);
    let first = ir.add_expr(IrExpr::UnitInstance);
    let second = ir.add_expr(IrExpr::UnitInstance);
    let construction = ir.add_expr(IrExpr::New {
        internal: copy_name,
        args: vec![first, second],
        ctor_params: None,
        declared_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    let identity = IrParameterIdentity::captured_value(
        None,
        0,
        IrValueCapture {
            declaration: IrCapturedDeclaration::ConstructorReceiver {
                owner,
                parameter: 1,
            },
            capturer: IrCapturingCallable::Lambda,
            crossinline_parameter: None,
        },
    );
    remove_capture(
        &mut ir,
        copy,
        construction,
        &[],
        CaptureRemoval {
            parameter: 0,
            field: Some(0),
            constructor_forest: &[],
        },
        copy_name,
    )
    .unwrap();
    assert_eq!(ir.classes[copy as usize].constructor_prefix_count, 1);
    assert!(ir.classes[copy as usize].ctor_args[0]
        .capture
        .as_ref()
        .unwrap()
        .receiver
        .is_some());
    assert_eq!(ir.checked_classifier_classes[&owner], original);
    assert_eq!(ir.classes[original as usize].constructor_prefix_count, 2);
    let crate::ir::IrParameterRole::CapturedValue { capture, .. } = identity.role else {
        panic!("value transport role retained")
    };
    assert_eq!(
        capture.declaration,
        IrCapturedDeclaration::ConstructorReceiver {
            owner,
            parameter: 1
        }
    );
    assert!(ir.classes[original as usize].ctor_args[1]
        .capture
        .as_ref()
        .unwrap()
        .receiver
        .is_some());
}

#[test]
fn copied_constructor_code_rewrites_a_shared_operand_once() {
    // The declaration takes `(a, lambda, b)`; the copy drops the lambda and takes `(a, b)`.
    let mut ir = crate::ir::IrFile::default();
    let name = crate::types::type_name("sample/InlineAnonymous");
    let mut class = crate::ir::IrClass::synthetic(name);
    class.is_anonymous_object = true;
    for _ in 0..3 {
        class.ctor_args.push(crate::ir::IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty: Ty::Int,
            declared_ty: None,
            is_field: false,
            field_index: None,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: crate::ir::IrCtorParameterProvenance::Value,
            capture_identity: None,
        });
    }
    let source = ir.add_class(class);
    // Parameter `b` and the first local, each read by both a super argument and the prelude.
    let parameter = ir.add_expr(IrExpr::GetValue(3));
    let local = ir.add_expr(IrExpr::GetValue(4));
    let spec = crate::ir::IrSpecializedAnonymousClass {
        source,
        order: 0,
        caller_declaration: crate::fir::DeclarationId::from_raw(1),
        caller: None,
        caller_is_default: false,
        caller_source_name: "caller".into(),
        inline_callee: crate::ir::IrInlineCallee::Module(crate::fir::CallableId::from_raw(0)),
        inline_callee_source_name: "callee".into(),
        inline_callee_default_entry: false,
        crossinline_operands: Vec::new(),
        parent: None,
        method_clones: Default::default(),
        bindings: Default::default(),
        reified_bindings: Default::default(),
        field_count: 0,
        property_count: 0,
        omitted_capture_fields: 0,
        constructor_parameters: vec![Some(0), Some(2)],
        caller_property_uses: Vec::new(),
        pending_property_roots: Vec::new(),
    };

    read_copy_parameters(
        &mut ir,
        source,
        &spec,
        &[parameter, local, parameter, local],
        name,
    )
    .expect("every read is of a parameter the copy keeps");

    assert!(matches!(ir.exprs[parameter as usize], IrExpr::GetValue(2)));
    assert!(matches!(ir.exprs[local as usize], IrExpr::GetValue(3)));
}

/// An anonymous object whose constructor prefix is `[fieldless, lambda, other]`: the first value
/// is kept in no field, the lambda is field 0 and the other capture field 1. kotlinc's first-use
/// order of the prefix is `[0, 2, 1]`.
fn object_with_a_fieldless_prefix_capture(ir: &mut crate::ir::IrFile) -> (ClassId, ExprId) {
    let name = crate::types::type_name("sample/InlineAnonymous");
    let mut class = crate::ir::IrClass::synthetic(name);
    class.is_anonymous_object = true;
    for field in [None, Some(0), Some(1)] {
        class.ctor_args.push(crate::ir::IrCtorArg {
            name: None,
            context_kind: crate::types::ContextParameterKind::None,
            ty: Ty::Int,
            declared_ty: None,
            is_field: field.is_some(),
            field_index: field,
            has_default: false,
            is_vararg: false,
            type_param: None,
            check: None,
            anonymous_super_forward: None,
            capture: None,
            provenance: crate::ir::IrCtorParameterProvenance::Capture,
            capture_identity: None,
        });
    }
    class
        .fields
        .push(crate::ir::IrField::new("$lambda".into(), Ty::Int));
    class
        .fields
        .push(crate::ir::IrField::new("$other".into(), Ty::Int));
    class.constructor_prefix_count = 3;
    class.ctor_param_count = 3;
    let class = ir.add_class(class);
    ir.class_capture_order
        .insert(class, vec![0, 2, 1].into_boxed_slice());
    let args = (0..3)
        .map(|_| ir.add_expr(IrExpr::UnitInstance))
        .collect::<Vec<_>>();
    let construction = ir.add_expr(IrExpr::New {
        internal: name,
        args,
        ctor_params: None,
        declared_params: None,
        ctor_desc: None,
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 0,
    });
    (class, construction)
}

#[test]
fn removing_a_capture_after_a_fieldless_one_keeps_the_recorded_order_by_constructor_identity() {
    let mut ir = crate::ir::IrFile::default();
    let (class, construction) = object_with_a_fieldless_prefix_capture(&mut ir);
    let name = ir.classes[class as usize].fq_name;

    remove_capture(
        &mut ir,
        class,
        construction,
        &[],
        CaptureRemoval {
            parameter: 1,
            field: Some(0),
            constructor_forest: &[],
        },
        name,
    )
    .unwrap();

    assert_eq!(&*ir.class_capture_order[&class], &[0, 1]);
    assert_eq!(ir.classes[class as usize].ctor_args[1].field_index, Some(0));
}

#[test]
fn lifted_captures_after_a_fieldless_one_extend_the_recorded_order_by_constructor_identity() {
    let mut ir = crate::ir::IrFile::default();
    let (class, construction) = object_with_a_fieldless_prefix_capture(&mut ir);
    let name = ir.classes[class as usize].fq_name;
    let lifted = ir.add_expr(IrExpr::UnitInstance);

    let field_base = capturing_lambdas::append_lifted_captures(
        &mut ir,
        class,
        construction,
        &[lifted],
        &[Ty::Int],
        &["$lifted".to_string()],
        name,
    )
    .unwrap();

    assert_eq!(field_base, 2);
    assert_eq!(&*ir.class_capture_order[&class], &[0, 2, 1, 3]);
    assert_eq!(ir.classes[class as usize].ctor_args[3].field_index, Some(2));
}

/// Constructor code that invokes a fieldless lambda parameter and also captures it in a nested
/// lambda keeps the parameter: the capture operand is evaluated in the constructor even though
/// the nested lambda's body is its own frame.
#[test]
fn a_lambda_parameter_captured_by_a_nested_lambda_is_not_consumed() {
    let mut ir = crate::ir::IrFile::default();
    let invoked = ir.add_expr(IrExpr::GetValue(1));
    let invocation = ir.add_expr(IrExpr::InvokeFunction {
        func: invoked,
        args: Vec::new(),
        params: Vec::new(),
        ret: Ty::String,
    });
    let unit = ir.add_expr(IrExpr::UnitInstance);
    let captured = ir.add_expr(IrExpr::GetValue(1));
    let nested = ir.add_expr(IrExpr::Lambda {
        impl_fn: 0,
        arity: 0,
        captures: vec![captured],
        sam: None,
        inline_body: Some(unit),
    });
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![invocation, nested],
        value: None,
    });
    assert!(parameter_lambda_uses(&ir, body, 1).is_none());

    let only_invoked = ir.add_expr(IrExpr::Block {
        stmts: vec![invocation],
        value: None,
    });
    let uses = parameter_lambda_uses(&ir, only_invoked, 1).expect("the parameter is only invoked");
    assert_eq!(uses.invocations, [invocation]);
    assert!(uses.forwards.is_empty());
}
