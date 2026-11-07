use super::*;
use krusty::fir::DeclarationId;
use krusty::ir::{
    FnParamInfo, IrCapturedDeclaration, IrCapturingCallable, IrCtorArg, IrCtorParameterProvenance,
    IrFunction, IrValueCapture,
};
use krusty::types::{ContextParameterKind, Ty};

pub(crate) fn original_prefix() -> (IrFile, DeclarationId, u32) {
    let mut ir = IrFile::default();
    let owner = DeclarationId::from_raw(11);
    let outer = krusty::types::type_name("Outer");
    let mut class = IrClass::synthetic(krusty::types::type_name("Original"));
    class.constructor_prefix_count = 1;
    class.ctor_args.push(IrCtorArg {
        name: None,
        context_kind: ContextParameterKind::None,
        ty: Ty::obj_name(outer),
        declared_ty: None,
        is_field: false,
        field_index: None,
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture: Some(IrConstructorCapture {
            source_name: "outer".into(),
            receiver: Some(IrCapturedReceiver::Enclosing {
                classifier: outer,
                owner: None,
            }),
            crossinline_parameter: None,
        }),
        provenance: IrCtorParameterProvenance::Capture,
        capture_identity: Some(krusty::fir::ClassCaptureIdentity::EnclosingClass(outer)),
    });
    let class = ir.add_class(class);
    ir.checked_classifier_classes.insert(owner, class);
    (ir, owner, class)
}

pub(crate) fn identity(owner: DeclarationId, parameter: u32) -> IrParameterIdentity {
    IrParameterIdentity::captured_value(
        None,
        0,
        IrValueCapture {
            declaration: IrCapturedDeclaration::ConstructorReceiver { owner, parameter },
            capturer: IrCapturingCallable::Lambda,
            crossinline_parameter: None,
        },
    )
}

#[test]
fn fieldless_prefix_keeps_distinct_local_and_constructor_surfaces() {
    let (mut ir, owner, class) = original_prefix();
    let names = constructor_receiver(&ir, owner, 0).expect("exact receiver prefix");
    assert_eq!(
        (names.field, names.parameter),
        ("this$0".into(), "$receiver".into())
    );
    let parameter = identity(owner, 0);
    let ty = ir.classes[class as usize].ctor_args[0].ty;
    let function = ir.add_fun(IrFunction {
        name: "helper".into(),
        params: vec![ty],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None],
    });
    ir.fn_params
        .insert(function, FnParamInfo::identities(vec![parameter.clone()]));
    let locals = crate::parameter_names::function_locals(&ir, function, &[ty]);
    assert_eq!(locals, Some(vec![Some("this$0".into())]));
    let parameters = SuspendLambdaParameters::new(vec![parameter.clone()], 0, vec![], None);
    let names = suspend_lambda_capture(&ir, &parameters, &parameter);
    assert_eq!(
        (names.field, names.parameter),
        ("this$0".into(), "$receiver".into())
    );
}

#[test]
fn original_prefix_identity_survives_copy_transport_removal() {
    let (mut ir, owner, class) = original_prefix();
    let function = ir.add_fun(IrFunction {
        name: "helper".into(),
        params: vec![ir.classes[class as usize].ctor_args[0].ty],
        ret: Ty::Unit,
        body: None,
        is_static: true,
        dispatch_receiver: None,
        param_checks: vec![None],
    });
    ir.fn_params
        .insert(function, FnParamInfo::identities(vec![identity(owner, 0)]));
    let shape = ir.functions[function as usize].clone();
    let cloned =
        krusty::ir::test_support::clone_class_method(&mut ir, function, shape, &Default::default());
    let mut copy = ir.classes[class as usize].clone();
    copy.fq_name = krusty::types::type_name("SpecializedCopy");
    copy.ctor_args.remove(0);
    copy.constructor_prefix_count = 0;
    ir.add_class(copy);
    assert_eq!(
        ir.function_parameter_identities(cloned),
        ir.function_parameter_identities(function)
    );
    let names = constructor_receiver(&ir, owner, 0).expect("original prefix remains authentic");
    assert_eq!(names.field, "this$0");
    assert_eq!(ir.checked_classifier_classes[&owner], class);
}

#[test]
fn full_prefix_order_disambiguates_equal_receiver_spellings() {
    let (mut ir, owner, class) = original_prefix();
    let mut second = ir.classes[class as usize].ctor_args[0].clone();
    let other = krusty::types::type_name("OtherOuter");
    second.ty = Ty::obj_name(other);
    second.capture.as_mut().unwrap().receiver = Some(IrCapturedReceiver::Enclosing {
        classifier: other,
        owner: None,
    });
    second.capture_identity = Some(krusty::fir::ClassCaptureIdentity::EnclosingClass(other));
    ir.classes[class as usize].ctor_args.push(second);
    ir.classes[class as usize].constructor_prefix_count = 2;
    ir.class_capture_order.insert(class, vec![1, 0].into());
    assert_eq!(constructor_receiver(&ir, owner, 1).unwrap().field, "this$0");
    assert_eq!(
        constructor_receiver(&ir, owner, 0).unwrap().field,
        "this$0$1"
    );
}

#[test]
fn exact_owner_and_role_prevent_same_typed_value_reinterpretation() {
    let (mut ir, owner, class) = original_prefix();
    assert!(constructor_receiver(&ir, DeclarationId::from_raw(12), 0).is_none());
    assert!(constructor_receiver(&ir, owner, 1).is_none());
    let argument = &mut ir.classes[class as usize].ctor_args[0];
    argument.capture.as_mut().unwrap().receiver = None;
    assert!(constructor_receiver(&ir, owner, 0).is_none());
    let argument = &mut ir.classes[class as usize].ctor_args[0];
    argument.capture = None;
    argument.provenance = IrCtorParameterProvenance::EnclosingInstance;
    assert!(constructor_receiver(&ir, owner, 0).is_none());
}

#[test]
fn exact_enum_entry_owner_uses_the_existing_checked_owner_alternative() {
    let (mut ir, owner, class) = original_prefix();
    ir.checked_classifier_classes.remove(&owner);
    ir.checked_enum_entry_classes.insert(owner, class);
    assert_eq!(constructor_receiver(&ir, owner, 0).unwrap().field, "this$0");
}

#[test]
fn same_typed_callable_receiver_keeps_its_recorded_role() {
    let (mut ir, owner, class) = original_prefix();
    ir.classes[class as usize].ctor_args[0]
        .capture
        .as_mut()
        .unwrap()
        .receiver = Some(IrCapturedReceiver::Callable {
        label: "extension".into(),
        owner: CapturedCallableOwner::Declaration,
    });
    let names = constructor_receiver(&ir, owner, 0).unwrap();
    assert_eq!(
        (names.field, names.parameter),
        ("$this_extension".into(), "$receiver".into())
    );
}
