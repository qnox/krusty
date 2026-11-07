use super::*;
use crate::fir::{DeclarationId, EnclosingInstanceOwner};
use crate::ir::{
    IrClass, IrConstructorCapture, IrConstructorTarget, IrCtorArg, IrField, IrSecondaryCtor,
};
use crate::types::type_name;

fn capture(entry: u32, field_index: Option<u32>) -> IrCtorArg {
    IrCtorArg {
        name: None,
        context_kind: crate::types::ContextParameterKind::None,
        ty: Ty::obj("Choice"),
        declared_ty: None,
        is_field: field_index.is_some(),
        field_index,
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture: Some(IrConstructorCapture {
            source_name: "".into(),
            crossinline_parameter: None,
            receiver: Some(IrCapturedReceiver::Enclosing {
                classifier: type_name("Choice"),
                owner: Some(EnclosingInstanceOwner {
                    declaration: DeclarationId::from_raw(entry),
                    kind: EnclosingInstanceKind::EnumEntry,
                }),
            }),
        }),
        provenance: IrCtorParameterProvenance::Capture,
        capture_identity: None,
    }
}

fn fixture() -> IrFile {
    let mut ir = IrFile::default();
    for (entry, name) in [(41, "Choice$X"), (42, "Choice$Y")] {
        let mut class = IrClass::synthetic(type_name(name));
        class.is_enum_entry = true;
        class.superclass = type_name("Choice");
        let class = ir.add_class(class);
        ir.checked_enum_entry_classes
            .insert(DeclarationId::from_raw(entry), class);
    }
    let mut class = IrClass::synthetic(type_name("Captured"));
    class.fields = vec![IrField::new("first".into(), Ty::obj("Choice"))];
    let mut value = capture(41, None);
    value.capture = None;
    value.ty = Ty::Int;
    value.provenance = IrCtorParameterProvenance::Value;
    class.ctor_args = vec![capture(41, Some(0)), capture(42, None), value];
    class.constructor_prefix_count = 2;
    class.ctor_param_count = 3;
    ir.add_class(class);
    ir
}

fn parameters() -> Vec<Ty> {
    vec![Ty::obj("Choice"), Ty::obj("Choice"), Ty::Int]
}

fn construction(ir: &mut IrFile, params: Option<Vec<Ty>>, descriptor: Option<&str>) -> u32 {
    ir.add_expr(IrExpr::New {
        internal: type_name("Captured"),
        args: Vec::new(),
        ctor_params: params,
        declared_params: None,
        ctor_desc: descriptor.map(Into::into),
        external_target: None,
        defaults: Box::new([]),
        default_prefix_count: 2,
    })
}

fn secondary(delegate: CtorDelegateTarget) -> IrSecondaryCtor {
    IrSecondaryCtor {
        annotations: Default::default(),
        source_order: 0,
        lines: Default::default(),
        prefix_params: parameters()[..2].to_vec(),
        params: vec![Ty::String],
        declared_spellings: Default::default(),
        named_params: Vec::new(),
        metadata_visibility: None,
        generated_debug: crate::ir::IrGeneratedDeclarationDebug::None,
        vararg_index: None,
        defaults: Vec::new(),
        delegate_prelude: Vec::new(),
        delegate_args: Vec::new(),
        default_parameters: Vec::new(),
        body: None,
        delegate,
        synthetic: false,
        platform_visible: false,
        vc_params: false,
        param_checks: Vec::new(),
    }
}

#[test]
fn realizes_exact_owners_in_stored_and_constructor_only_captures() {
    let mut ir = fixture();
    let explicit = construction(&mut ir, Some(parameters()), None);
    let primary = construction(&mut ir, None, None);
    realize(&mut ir).unwrap();
    let physical = [Ty::obj("Choice$X"), Ty::obj("Choice$Y"), Ty::Int];
    assert_eq!(
        ir.classes[2]
            .ctor_args
            .iter()
            .map(|arg| arg.ty)
            .collect::<Vec<_>>(),
        physical
    );
    assert_eq!(ir.classes[2].fields[0].ty, physical[0]);
    let IrExpr::New { ctor_params, .. } = ir.expr(explicit) else {
        panic!("construction")
    };
    assert_eq!(ctor_params.as_deref(), Some(physical.as_slice()));
    let IrExpr::New { ctor_params, .. } = ir.expr(primary) else {
        panic!("construction")
    };
    assert_eq!(*ctor_params, None);
    assert_eq!(
        ir.classes[2].ctor_args[0].capture,
        capture(41, Some(0)).capture
    );
}

#[test]
fn preserves_secondary_selection_and_source_parameters_in_all_local_delegations() {
    let mut ir = fixture();
    let target = IrConstructorTarget {
        ordinal: 1,
        ..IrConstructorTarget::UNRESTRICTED_PRIMARY
    };
    ir.classes[2]
        .secondary_ctors
        .push(secondary(CtorDelegateTarget::This {
            target_params: vec![Ty::Int],
            target,
            default_masks: Vec::new(),
        }));
    let mut subclass = IrClass::synthetic(type_name("Derived"));
    subclass.superclass = type_name("Captured");
    subclass.super_ctor_params = parameters();
    subclass.super_ctor = target;
    subclass
        .secondary_ctors
        .push(secondary(CtorDelegateTarget::Super {
            owner: type_name("Captured"),
            target_params: vec![Ty::obj("Choice"), Ty::obj("Choice"), Ty::String],
            target,
            default_masks: vec![1],
        }));
    ir.add_class(subclass);
    let call = construction(
        &mut ir,
        Some(vec![Ty::obj("Choice"), Ty::obj("Choice"), Ty::String]),
        None,
    );
    realize(&mut ir).unwrap();
    let prefix = [Ty::obj("Choice$X"), Ty::obj("Choice$Y")];
    assert_eq!(ir.classes[2].secondary_ctors[0].prefix_params, prefix);
    assert_eq!(ir.classes[2].secondary_ctors[0].params, [Ty::String]);
    let CtorDelegateTarget::This {
        target_params,
        target: selected,
        ..
    } = &ir.classes[2].secondary_ctors[0].delegate
    else {
        panic!("this")
    };
    assert_eq!(*selected, target);
    assert_eq!(*target_params, [Ty::Int]);
    assert_eq!(ir.classes[3].super_ctor, target);
    assert_eq!(
        ir.classes[3].super_ctor_params,
        [prefix[0], prefix[1], Ty::Int]
    );
    let CtorDelegateTarget::Super {
        target_params,
        target: selected,
        default_masks,
        ..
    } = &ir.classes[3].secondary_ctors[0].delegate
    else {
        panic!("super")
    };
    assert_eq!(*selected, target);
    assert_eq!(*target_params, [prefix[0], prefix[1], Ty::String]);
    assert_eq!(*default_masks, [1]);
    let IrExpr::New { ctor_params, .. } = ir.expr(call) else {
        panic!("construction")
    };
    assert_eq!(
        ctor_params.as_deref(),
        Some([prefix[0], prefix[1], Ty::String].as_slice())
    );
}

#[test]
fn zero_argument_this_delegation_forwards_only_its_separate_capture_prefix() {
    let mut ir = fixture();
    let class = &mut ir.classes[2];
    class.ctor_args.truncate(2);
    class.ctor_param_count = 2;
    let mut constructor = secondary(CtorDelegateTarget::This {
        target_params: Vec::new(),
        target: IrConstructorTarget::UNRESTRICTED_PRIMARY,
        default_masks: Vec::new(),
    });
    constructor.params.clear();
    class.secondary_ctors.push(constructor);
    realize(&mut ir).unwrap();
    assert_eq!(
        ir.classes[2].secondary_ctors[0].prefix_params,
        [Ty::obj("Choice$X"), Ty::obj("Choice$Y")]
    );
    let CtorDelegateTarget::This { target_params, .. } = &ir.classes[2].secondary_ctors[0].delegate
    else {
        panic!("this")
    };
    assert_eq!(*target_params, Vec::<Ty>::new());
}

#[test]
fn missing_entry_realization_fails_without_changing_the_semantic_type() {
    let mut ir = fixture();
    ir.checked_enum_entry_classes
        .remove(&DeclarationId::from_raw(42));
    assert_eq!(
        realize(&mut ir),
        Err("enclosing capture owner DeclarationId(42) has no realized enum-entry class".into())
    );
    assert_eq!(
        ir.classes[2]
            .ctor_args
            .iter()
            .map(|arg| arg.ty)
            .collect::<Vec<_>>(),
        parameters()
    );
}

#[test]
fn ordinary_enclosing_receivers_need_no_local_entry_class() {
    let mut ir = fixture();
    for argument in &mut ir.classes[2].ctor_args {
        if let Some(IrCapturedReceiver::Enclosing {
            owner: Some(owner), ..
        }) = argument
            .capture
            .as_mut()
            .and_then(|capture| capture.receiver.as_mut())
        {
            owner.kind = EnclosingInstanceKind::Classifier;
        }
    }
    ir.checked_enum_entry_classes.clear();
    realize(&mut ir).unwrap();
    assert_eq!(
        ir.classes[2]
            .ctor_args
            .iter()
            .map(|arg| arg.ty)
            .collect::<Vec<_>>(),
        parameters()
    );
    assert_eq!(ir.classes[2].fields[0].ty, Ty::obj("Choice"));
}

#[test]
fn a_late_incomplete_selected_constructor_publishes_no_slots() {
    let mut ir = fixture();
    construction(&mut ir, Some(vec![Ty::obj("Choice")]), None);
    let before = format!("{:?}", ir.classes[2]);
    assert_eq!(
        realize(&mut ir),
        Err("selected constructor has no enclosing capture prefix parameter 1".into())
    );
    assert_eq!(format!("{:?}", ir.classes[2]), before);
}

#[test]
fn an_encoded_constructor_is_not_guessed_from_its_class_name() {
    let mut ir = fixture();
    let call = construction(&mut ir, Some(parameters()), Some("(LChoice;LChoice;I)V"));
    assert_eq!(
        realize(&mut ir),
        Err("an enclosing capture construction has no typed local constructor boundary".into())
    );
    assert_eq!(ir.classes[2].ctor_args[0].ty, Ty::obj("Choice"));
    let IrExpr::New { ctor_desc, .. } = ir.expr(call) else {
        panic!("construction")
    };
    assert_eq!(ctor_desc.as_deref(), Some("(LChoice;LChoice;I)V"));
}

#[test]
fn an_entry_owner_requires_the_selected_semantic_enum() {
    let mut ir = fixture();
    ir.classes[2].ctor_args[0].ty = Ty::String;
    assert_eq!(
        realize(&mut ir),
        Err("an enclosing capture disagrees with its selected enum owner".into())
    );
    assert_eq!(ir.classes[2].fields[0].ty, Ty::obj("Choice"));
}

fn assert_declines_without_publication(ir: &mut IrFile, expected: &str) {
    let before = format!("{:?}\n{:?}", ir.classes, ir.exprs);
    assert_eq!(realize(ir), Err(expected.into()));
    assert_eq!(format!("{:?}\n{:?}", ir.classes, ir.exprs), before);
}

#[test]
fn a_wrong_paired_field_type_is_not_repaired_by_realization() {
    let mut ir = fixture();
    ir.classes[2].fields[0].ty = Ty::String;
    assert_declines_without_publication(
        &mut ir,
        "enclosing capture parameter 0 disagrees with its paired field type",
    );
}

#[test]
fn same_semantic_type_does_not_allow_two_entry_owners_to_share_a_field() {
    let mut ir = fixture();
    ir.classes[2].ctor_args[1].is_field = true;
    ir.classes[2].ctor_args[1].field_index = Some(0);
    assert_declines_without_publication(
        &mut ir,
        "enclosing capture field 0 has multiple constructor owners",
    );
}

#[test]
fn an_ordinary_parameter_cannot_alias_a_selected_capture_field() {
    let mut ir = fixture();
    ir.classes[2].ctor_args[2].is_field = true;
    ir.classes[2].ctor_args[2].field_index = Some(0);
    assert_declines_without_publication(
        &mut ir,
        "enclosing capture field 0 has multiple constructor owners",
    );
}

#[test]
fn a_wrong_secondary_prefix_type_publishes_no_slots() {
    let mut ir = fixture();
    let mut constructor = secondary(CtorDelegateTarget::This {
        target_params: vec![Ty::Int],
        target: IrConstructorTarget::UNRESTRICTED_PRIMARY,
        default_masks: Vec::new(),
    });
    constructor.prefix_params[1] = Ty::String;
    ir.classes[2].secondary_ctors.push(constructor);
    assert_declines_without_publication(
        &mut ir,
        "selected constructor enclosing capture parameter 1 has a different semantic type",
    );
}

#[test]
fn a_wrong_superclass_prefix_type_publishes_no_slots() {
    let mut ir = fixture();
    let mut subclass = IrClass::synthetic(type_name("Derived"));
    subclass.superclass = type_name("Captured");
    subclass.super_ctor_params = vec![Ty::String, Ty::obj("Choice"), Ty::Int];
    ir.add_class(subclass);
    assert_declines_without_publication(
        &mut ir,
        "selected constructor enclosing capture parameter 0 has a different semantic type",
    );
}

#[test]
fn a_wrong_selected_secondary_super_prefix_publishes_no_slots() {
    let mut ir = fixture();
    let mut subclass = IrClass::synthetic(type_name("Derived"));
    subclass
        .secondary_ctors
        .push(secondary(CtorDelegateTarget::Super {
            owner: type_name("Captured"),
            target_params: vec![Ty::obj("Choice"), Ty::String, Ty::Int],
            target: IrConstructorTarget::UNRESTRICTED_PRIMARY,
            default_masks: Vec::new(),
        }));
    ir.add_class(subclass);
    assert_declines_without_publication(
        &mut ir,
        "selected constructor enclosing capture parameter 1 has a different semantic type",
    );
}

#[test]
fn a_late_wrong_selected_construction_type_changes_no_earlier_consumer() {
    let mut ir = fixture();
    construction(&mut ir, Some(parameters()), None);
    construction(
        &mut ir,
        Some(vec![Ty::obj("Choice"), Ty::obj("Choice$X"), Ty::Int]),
        None,
    );
    assert_declines_without_publication(
        &mut ir,
        "selected constructor enclosing capture parameter 1 has a different semantic type",
    );
}
