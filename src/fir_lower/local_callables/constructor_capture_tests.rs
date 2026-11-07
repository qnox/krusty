use super::*;

#[test]
fn nested_recapture_preserves_original_constructor_declaration_key() {
    let owner = crate::fir::DeclarationId::from_raw(11);
    let source = crate::fir::FirCaptureSource::ConstructorPrefix { owner, field: 2 };
    let ty = crate::fir::ResolvedTy::new(Ty::obj("Outer")).unwrap();
    let original = crate::ir::IrCapturedDeclaration::ConstructorReceiver {
        owner,
        parameter: 2,
    };
    let slots = HashMap::from_iter([(
        (0, source),
        CaptureSlot {
            slot: 7,
            ty,
            shared_cell: false,
            stable: true,
            crossinline_parameter: None,
            declared: CapturedDeclaration {
                name: None,
                declaration: original,
            },
        },
    )]);
    let body = FirBody::new(crate::fir::BodyOwnerId::from_raw(99));
    let capture = crate::fir::FirCapture {
        origin: crate::fir::OriginId::from_raw(0),
        enclosing_depth: 1,
        source,
        ty,
        shared_cell: false,
    };
    let declared = captured_declaration(&crate::ir::IrFile::default(), &body, &slots, &capture)
        .expect("recapture uses the existing authenticated slot, not body99's constructor");
    assert_eq!(declared.name, None);
    assert_eq!(declared.declaration, original);
}
