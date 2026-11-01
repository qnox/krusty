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
