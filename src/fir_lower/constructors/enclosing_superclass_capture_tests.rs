//! Constructor lowering reads a superclass capture from the enclosing instance by closure identity.

use crate::fir::ClassCaptureIdentity;
use crate::ir::{
    IrCapturedReceiver, IrClass, IrConstructorCapture, IrCtorArg, IrCtorParameterProvenance,
    IrExpr, IrFile,
};
use crate::types::{ContextParameterKind, Ty};

fn argument(
    ty: Ty,
    provenance: IrCtorParameterProvenance,
    field_index: Option<u32>,
    capture: Option<IrConstructorCapture>,
    capture_identity: Option<ClassCaptureIdentity>,
) -> IrCtorArg {
    IrCtorArg {
        name: None,
        context_kind: ContextParameterKind::None,
        ty,
        declared_ty: None,
        is_field: field_index.is_some(),
        field_index,
        has_default: false,
        is_vararg: false,
        type_param: None,
        check: None,
        anonymous_super_forward: None,
        capture,
        provenance,
        capture_identity,
    }
}

fn capture(field: u32, identity: ClassCaptureIdentity) -> IrCtorArg {
    argument(
        Ty::String,
        IrCtorParameterProvenance::Capture,
        Some(field),
        Some(IrConstructorCapture {
            source_name: "same".into(),
            receiver: Some(IrCapturedReceiver::Callable {
                label: "same".into(),
                owner: crate::fir::CapturedCallableOwner::Declaration,
            }),
            crossinline_parameter: None,
        }),
        Some(identity),
    )
}

#[test]
fn inner_super_argument_follows_identity_and_enclosing_parameter_role() {
    let wanted = ClassCaptureIdentity::Receiver(7);
    let same_label_other_receiver = ClassCaptureIdentity::Receiver(8);
    let mut ir = IrFile::default();

    let mut parent = IrClass::synthetic("Local".into());
    parent.constructor_prefix_count = 1;
    parent.ctor_args.push(capture(0, wanted));
    let parent = ir.add_class(parent);
    ir.class_capture_identities.insert((parent, 0), wanted);

    let mut enclosing = IrClass::synthetic("Holder".into());
    // Both captures have the same source/debug label. Identity deliberately selects the second.
    enclosing
        .ctor_args
        .push(capture(0, same_label_other_receiver));
    enclosing.ctor_args.push(capture(1, wanted));
    let enclosing = ir.add_class(enclosing);

    let mut inner = IrClass::synthetic("Holder$Inner".into());
    inner.is_inner_class = true;
    inner.superclass = ir.classes[parent as usize].fq_name;
    // An unnamed synthetic value before the enclosing instance proves that absence of a source
    // name is not used as the role. The enclosing instance is value slot 2, not slot 1.
    inner.ctor_args.push(argument(
        Ty::Int,
        IrCtorParameterProvenance::Value,
        None,
        None,
        None,
    ));
    inner.ctor_args.push(argument(
        Ty::obj("Holder"),
        IrCtorParameterProvenance::EnclosingInstance,
        Some(0),
        None,
        None,
    ));
    let inner = ir.add_class(inner);

    super::finalize_local_superclass_captures(&mut ir).expect("finalize superclass capture");

    assert_eq!(ir.classes[inner as usize].super_ctor_params, [Ty::String]);
    let argument = ir.classes[inner as usize].super_args[0];
    let IrExpr::GetField {
        receiver,
        class,
        index,
    } = ir.expr(argument)
    else {
        panic!("super capture was not read from the enclosing instance");
    };
    assert_eq!((*class, *index), (enclosing, 1));
    assert!(matches!(ir.expr(*receiver), IrExpr::GetValue(2)));
    assert_eq!(
        ir.classes[*class as usize].ctor_args[*index as usize].capture_identity,
        Some(wanted)
    );
}

fn explicit_projection() -> (IrFile, crate::ir::IrCheckedConstructorBody, u32, u32) {
    let mut ir = IrFile::default();
    let wanted = ClassCaptureIdentity::Receiver(17);
    let actual = ClassCaptureIdentity::Receiver(18);
    let mut parent = IrClass::synthetic("Parent".into());
    parent.constructor_prefix_count = 1;
    parent
        .fields
        .push(crate::ir::IrField::new("value".into(), Ty::String));
    parent.ctor_args.push(capture(0, wanted));
    let parent = ir.add_class(parent);
    ir.class_capture_identities.insert((parent, 0), wanted);
    let mut child = IrClass::synthetic("Child".into());
    child.superclass = ir.classes[parent as usize].fq_name;
    child.constructor_prefix_count = 1;
    child
        .fields
        .push(crate::ir::IrField::new("value".into(), Ty::String));
    child.ctor_args.push(capture(0, actual));
    let child = ir.add_class(child);
    ir.class_capture_identities.insert((child, 0), actual);
    let parameter = ir.add_expr(IrExpr::GetValue(1));
    let constructor = crate::ir::IrCheckedConstructorBody {
        class: child,
        ordinal: 0,
        annotations: Default::default(),
        parameters: Vec::new(),
        defaults: Vec::new(),
        delegation: None,
        body: None,
        body_attached: true,
    };
    (ir, constructor, parent, parameter)
}

#[test]
fn an_explicit_super_capture_reads_the_parameter_without_retagging_the_child() {
    let (mut ir, constructor, parent, parameter) = explicit_projection();
    let original = ir.class_capture_identities[&(constructor.class, 0)];
    let mut arguments = Vec::new();
    let mut parameters = Vec::new();
    assert_eq!(
        super::project_delegation_captures(
            &mut ir,
            &constructor,
            Some(parent),
            Some(vec![(parameter, Ty::String)]),
            &mut arguments,
            &mut parameters,
            crate::fir::DeclarationId::from_raw(0),
        )
        .expect("checked explicit capture"),
        1
    );
    assert_eq!(arguments, [parameter]);
    assert_eq!(parameters, [Ty::String]);
    assert!(matches!(ir.expr(arguments[0]), IrExpr::GetValue(1)));
    assert_eq!(
        ir.class_capture_identities[&(constructor.class, 0)],
        original
    );
    assert_ne!(original, ir.class_capture_identities[&(parent, 0)]);
    ir.classes[constructor.class as usize].super_args = arguments.clone();
    ir.classes[constructor.class as usize].super_ctor_params = parameters;
    super::finalize_local_superclass_captures(&mut ir).expect("complete prefix needs no inference");
    assert_eq!(ir.classes[constructor.class as usize].super_args, arguments);
    assert_eq!(ir.classes[constructor.class as usize].ctor_args.len(), 1);
    assert_eq!(
        ir.class_capture_identities[&(constructor.class, 0)],
        original
    );
}

#[test]
fn explicit_shared_super_prefix_retains_the_targets_holder_role() {
    let (mut ir, mut constructor, parent, parameter) = explicit_projection();
    ir.shared_class_capture_fields
        .insert((parent, 0), Ty::String);
    for ordinal in [0, 1] {
        constructor.ordinal = ordinal;
        super::project_delegation_captures(
            &mut ir,
            &constructor,
            Some(parent),
            Some(vec![(parameter, Ty::String)]),
            &mut Vec::new(),
            &mut Vec::new(),
            crate::fir::DeclarationId::from_raw(0),
        )
        .expect("raw constructor parameter carries the same shared holder");
    }
    assert_eq!(
        ir.shared_super_capture_parameters[&(constructor.class, 0)],
        Ty::String
    );
    assert_eq!(
        ir.shared_secondary_super_capture_parameters[&(constructor.class, 0, 0)],
        Ty::String
    );
}

#[test]
fn an_invalid_explicit_projection_is_not_repaired_by_closure_inference() {
    for wrong in [Vec::new(), vec![(0, Ty::Int)]] {
        let (mut ir, constructor, parent, _) = explicit_projection();
        let mut arguments = Vec::new();
        let mut parameters = Vec::new();
        assert!(super::project_delegation_captures(
            &mut ir,
            &constructor,
            Some(parent),
            Some(wrong),
            &mut arguments,
            &mut parameters,
            crate::fir::DeclarationId::from_raw(0),
        )
        .is_err());
        assert!(arguments.is_empty());
        assert!(parameters.is_empty());
        assert!(ir.shared_super_capture_parameters.is_empty());
    }
}
