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
            receiver: Some(IrCapturedReceiver::Callable("same".into())),
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
