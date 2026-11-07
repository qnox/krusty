//! Captured receiver ordinals name the receiver ledger, independently of value-prefix slots.

use super::*;
use crate::klib_lowering::tests::local_symbol as symbol;
use crate::metadata::klib_ir::KlibIrSymbolKind;

#[test]
fn a_receiver_after_a_value_capture_keeps_its_origin_through_nested_recapture() {
    let receiver = symbol(KlibIrSymbolKind::ReceiverParameter, 1);
    let value = symbol(KlibIrSymbolKind::ValueParameter, 2);
    let mut header = FunctionHeader {
        name: "factory".to_owned(),
        params: vec![Ty::String, Ty::Int],
        ret: Ty::Int,
        identities: vec![
            IrParameterIdentity::extension_receiver(),
            IrParameterIdentity::producer_value("number"),
        ],
        context_count: 0,
        extension_receiver: true,
        dispatch_receiver: None,
        receiver_slot: None,
        value_receiver: None,
        singleton_extension_unread_receiver: None,
        values: [
            (
                receiver.clone(),
                ParameterValue {
                    index: 0,
                    stable_read: true,
                    erased: None,
                },
            ),
            (
                value.clone(),
                ParameterValue {
                    index: 1,
                    stable_read: true,
                    erased: None,
                },
            ),
        ]
        .into_iter()
        .collect(),
        defaults: vec![None, None],
        type_parameters: KlibTypeParameters::default(),
        erasures: HashMap::default(),
        erased_result: None,
    };
    let captured_value = header
        .capture_parameter(&value, 0, IrCapturingCallable::Lambda, &[], false)
        .unwrap()
        .unwrap();
    let captured_receiver = header
        .capture_parameter(&receiver, 1, IrCapturingCallable::Lambda, &[], false)
        .unwrap()
        .unwrap();
    assert!(matches!(
        captured_value.identity.role,
        IrParameterRole::CapturedValue { ordinal: 0, .. }
    ));
    assert_eq!(
        captured_receiver.identity.role,
        IrParameterRole::CapturedReceiver { ordinal: 0 }
    );
    let origin = IrCapturedReceiver::Callable {
        label: "factory".into(),
        owner: crate::fir::CapturedCallableOwner::Declaration,
    };
    assert_eq!(captured_receiver.receiver, Some(origin.clone()));
    // The lifted frame stores a value first, then its receiver. Recapture must use the old
    // receiver ledger entry, not reinterpret it as the nested closure's own receiver.
    header.params = vec![Ty::Int, Ty::String];
    header.identities = vec![captured_value.identity, captured_receiver.identity];
    header.values.get_mut(&value).unwrap().index = 0;
    header.values.get_mut(&receiver).unwrap().index = 1;
    header.extension_receiver = false;
    let recaptured = header
        .capture_parameter(
            &receiver,
            1,
            IrCapturingCallable::Lambda,
            std::slice::from_ref(&origin),
            true,
        )
        .unwrap()
        .unwrap();
    assert_eq!(recaptured.ty, Ty::String);
    assert_eq!(
        recaptured.identity.role,
        IrParameterRole::CapturedReceiver { ordinal: 0 }
    );
    assert_eq!(recaptured.receiver, Some(origin));
}

fn instance_header() -> (FunctionHeader, KlibIrSymbol, KlibIrSymbol, Ty) {
    let receiver = symbol(KlibIrSymbolKind::ReceiverParameter, 10);
    let value = symbol(KlibIrSymbolKind::ValueParameter, 11);
    let formal = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
    let receiver_ty = Ty::obj_args("demo/Container", &[formal]);
    let header = FunctionHeader {
        name: "member".into(),
        params: vec![formal],
        ret: formal,
        identities: vec![IrParameterIdentity::source("value")],
        context_count: 0,
        extension_receiver: false,
        dispatch_receiver: Some(receiver_ty),
        receiver_slot: None,
        value_receiver: Some(receiver_ty),
        singleton_extension_unread_receiver: None,
        values: [
            (
                receiver.clone(),
                ParameterValue {
                    index: 0,
                    stable_read: true,
                    erased: None,
                },
            ),
            (
                value.clone(),
                ParameterValue {
                    index: 1,
                    stable_read: true,
                    erased: None,
                },
            ),
        ]
        .into_iter()
        .collect(),
        defaults: vec![None],
        type_parameters: KlibTypeParameters::default(),
        erasures: HashMap::default(),
        erased_result: None,
    };
    (header, receiver, value, receiver_ty)
}

#[test]
fn explicit_instance_prefix_never_consumes_the_following_parameter_identity() {
    for prepared_inline in [false, true] {
        let (mut header, receiver, value, mut receiver_ty) = instance_header();
        let classifier = receiver_ty.kotlin_class_internal().unwrap();
        if prepared_inline {
            header.dispatch_receiver = None;
            header.receiver_slot = Some(classifier);
            header.value_receiver = None;
            receiver_ty = Ty::obj_name(classifier);
        }
        assert_eq!(header.parameter_count(), 2);
        let captured_receiver = header
            .capture_parameter(&receiver, 0, IrCapturingCallable::Lambda, &[], false)
            .unwrap()
            .unwrap();
        assert_eq!(captured_receiver.ty, receiver_ty);
        assert_eq!(
            captured_receiver.identity.role,
            IrParameterRole::CapturedReceiver { ordinal: 0 }
        );
        assert_eq!(captured_receiver.identity.source_name, None);
        assert_eq!(
            captured_receiver.receiver,
            Some(IrCapturedReceiver::Enclosing {
                classifier,
                owner: None
            })
        );
        let captured_value = header
            .capture_parameter(&value, 1, IrCapturingCallable::Lambda, &[], false)
            .unwrap()
            .unwrap();
        assert_eq!(captured_value.ty, header.params[0]);
        assert_eq!(captured_value.receiver, None);
        assert_eq!(
            captured_value.identity.source_name.as_deref(),
            Some("value")
        );
        let IrParameterRole::CapturedValue { ordinal, capture } = captured_value.identity.role
        else {
            panic!("the following source parameter is a captured value")
        };
        assert_eq!(ordinal, 1);
        assert_eq!(capture.declaration, IrCapturedDeclaration::Parameter);
    }
}

#[test]
fn value_instance_and_parameter_keep_roles_through_nested_recapture() {
    let (mut header, receiver, value, receiver_ty) = instance_header();
    let captured_receiver = header
        .capture_parameter(&receiver, 0, IrCapturingCallable::Lambda, &[], false)
        .unwrap()
        .unwrap();
    let captured_value = header
        .capture_parameter(&value, 1, IrCapturingCallable::Lambda, &[], false)
        .unwrap()
        .unwrap();
    let origin = captured_receiver.receiver.clone().unwrap();
    header.params = vec![receiver_ty, captured_value.ty];
    header.identities = vec![captured_receiver.identity, captured_value.identity.clone()];
    header.dispatch_receiver = None;
    header.value_receiver = None;
    header.defaults = vec![None, None];
    let recaptured_receiver = header
        .capture_parameter(
            &receiver,
            0,
            IrCapturingCallable::Lambda,
            std::slice::from_ref(&origin),
            true,
        )
        .unwrap()
        .unwrap();
    assert_eq!(recaptured_receiver.ty, receiver_ty);
    assert_eq!(recaptured_receiver.receiver, Some(origin));
    assert_eq!(
        recaptured_receiver.identity.role,
        IrParameterRole::CapturedReceiver { ordinal: 0 }
    );
    let recaptured_value = header
        .capture_parameter(&value, 1, IrCapturingCallable::Lambda, &[], true)
        .unwrap()
        .unwrap();
    assert_eq!(recaptured_value.ty, captured_value.ty);
    assert_eq!(recaptured_value.identity, captured_value.identity);
    assert_eq!(recaptured_value.receiver, None);
}

#[test]
fn a_same_typed_parameter_without_instance_role_stays_a_value() {
    let (mut header, receiver, value, receiver_ty) = instance_header();
    header.dispatch_receiver = None;
    header.value_receiver = None;
    header.params = vec![receiver_ty];
    header.identities = vec![IrParameterIdentity::source("ordinary")];
    header.values.remove(&receiver);
    header.values.get_mut(&value).unwrap().index = 0;
    let capture = header
        .capture_parameter(&value, 0, IrCapturingCallable::Lambda, &[], false)
        .unwrap()
        .unwrap();
    assert_eq!(capture.ty, receiver_ty);
    assert_eq!(capture.receiver, None);
    assert_eq!(capture.identity.source_name.as_deref(), Some("ordinary"));
    assert!(matches!(
        capture.identity.role,
        IrParameterRole::CapturedValue { ordinal: 0, .. }
    ));
}
