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
    remove_capture(&mut ir, copy, construction, &[], 0, 0, copy_name).unwrap();
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
