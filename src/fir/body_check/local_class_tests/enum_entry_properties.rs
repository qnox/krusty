use super::*;

#[test]
fn the_whole_spilled_write_program_captures_the_exact_entry_instance() {
    let (index, bodies) = checked_streamed_bodies(include_str!(
        "../../../../tests/box_regressions/enumEntryPropertyReceiverWrites.kt"
    ));
    let classifier = crate::types::type_name("EntryWrites");
    let owner = index
        .classifier_declaration(classifier)
        .expect("enum declaration");
    let entry = index
        .owned_declaration(owner, DeclarationKind::EnumEntry, 0)
        .expect("selected entry declaration");
    let expected = crate::fir::EnclosingInstanceOwner {
        declaration: entry,
        kind: crate::fir::EnclosingInstanceKind::EnumEntry,
    };
    let objects = bodies
        .iter()
        .flat_map(|body| {
            (0..body.expression_count()).filter_map(move |raw| {
                let expression = body.expr(FirExprId::from_raw(raw as u32))?;
                match &expression.kind {
                    FirExprKind::AnonymousObject(object) => Some(object),
                    _ => None,
                }
            })
        })
        .collect::<Vec<_>>();
    let [object] = objects.as_slice() else {
        panic!("one whole-program anonymous construction expected")
    };
    let [capture] = object.captures.as_ref() else {
        panic!(
            "the declared property must travel through its one entry receiver: {:?}",
            object.captures
        )
    };
    assert_eq!(capture.ty.get(), Ty::obj_name(classifier));
    assert!(!capture.shared_cell);
    assert_eq!(
        capture.receiver,
        Some(crate::fir::FirCapturedReceiver::Enclosing {
            classifier,
            owner: Some(expected),
        })
    );
}

#[test]
fn the_whole_custom_setter_program_keeps_an_ordinary_property_write() {
    let (index, bodies) = checked_streamed_bodies(include_str!(
        "../../../../tests/box_regressions/enumEntryPropertyCustomSetter.kt"
    ));
    let classifier = index
        .classifier_declaration(crate::types::type_name("EntrySetter"))
        .expect("enum declaration");
    let entry = index
        .owned_declaration(classifier, DeclarationKind::EnumEntry, 0)
        .expect("selected entry");
    let declaration = index
        .owned_declaration(entry, DeclarationKind::Property, 0)
        .expect("selected mutable property");
    let property = index
        .property_for_declaration(declaration)
        .expect("stable property");
    assert!(index.property(property).unwrap().mutable);
    let mut writes = Vec::new();
    for body in &bodies {
        for raw in 0..body.expression_count() {
            match &body.expr(FirExprId::from_raw(raw as u32)).unwrap().kind {
                FirExprKind::PropertyWrite {
                    target:
                        FirPropertyTarget::Module {
                            property: selected, ..
                        },
                    dispatch_receiver,
                    ..
                } if *selected == property => writes.push(dispatch_receiver.is_some()),
                _ => {}
            }
        }
    }
    assert_eq!(writes, [true]);
}

#[test]
fn the_whole_function_property_program_publishes_its_selected_entry_reads() {
    let (index, bodies) = checked_streamed_bodies(include_str!(
        "../../../../tests/box_regressions/enumEntryReceiverFunctionProperty.kt"
    ));
    let classifier = index
        .classifier_declaration(crate::types::type_name("EntryFunctionValue"))
        .expect("enum declaration");
    let entry = index
        .owned_declaration(classifier, DeclarationKind::EnumEntry, 0)
        .expect("selected entry");
    let selected = (0..3)
        .map(|sibling| {
            let declaration = index
                .owned_declaration(entry, DeclarationKind::Property, sibling)
                .expect("selected callable property declaration");
            index
                .property_for_declaration(declaration)
                .expect("selected property")
        })
        .collect::<Vec<_>>();
    let mut reads = vec![0; selected.len()];
    for body in &bodies {
        for raw in 0..body.expression_count() {
            if let FirExprKind::PropertyRead {
                target: FirPropertyTarget::Module { property, .. },
                dispatch_receiver,
                ..
            } = &body.expr(FirExprId::from_raw(raw as u32)).unwrap().kind
            {
                if let Some(slot) = selected.iter().position(|selected| selected == property) {
                    assert!(dispatch_receiver.is_some(), "selected entry receiver");
                    reads[slot] += 1;
                }
            }
        }
    }
    assert_eq!(reads, [4, 2, 1]);
}
