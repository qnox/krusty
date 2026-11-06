use super::*;

#[test]
fn forwarded_nested_object_reads_the_actual_enclosing_capture_owner() {
    let source =
        include_str!("../../../../tests/box_regressions/captureStorageForwardedNestedObject.kt");
    let (index, bodies) = checked_streamed_bodies(source);
    let selected = |name| {
        bodies
            .iter()
            .filter(|body| {
                index.declaration_name(DeclarationId::from_raw(body.owner().raw())) == Some(name)
            })
            .collect::<Vec<_>>()
    };
    let makers = selected("make");
    let [maker] = makers.as_slice() else {
        panic!("one enclosing construction body expected")
    };
    let outer = (0..maker.expression_count())
        .filter_map(
            |raw| match &maker.expr(FirExprId::from_raw(raw as u32))?.kind {
                FirExprKind::AnonymousObject(object) => Some(object),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    let [outer] = outer.as_slice() else {
        panic!("one enclosing anonymous object expected")
    };
    let [capture] = outer.captures.as_ref() else {
        panic!("one lexical capture expected")
    };
    assert!(
        capture.stored,
        "the later member invocation needs the enclosing field"
    );
    assert_eq!(
        capture.source,
        FirLocalClassCaptureSource::Value(maker.parameters()[0].value)
    );
    let readers = selected("read");
    let [reader] = readers.as_slice() else {
        panic!("one later member body expected")
    };
    let nested = (0..reader.expression_count())
        .filter_map(
            |raw| match &reader.expr(FirExprId::from_raw(raw as u32))?.kind {
                FirExprKind::AnonymousObject(object) => Some(object),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(nested.len(), 2);
    let captures = nested
        .iter()
        .flat_map(|object| object.captures.iter())
        .collect::<Vec<_>>();
    let [capture] = captures.as_slice() else {
        panic!("only the callback stores the forwarded value")
    };
    assert!(capture.stored);
    assert_eq!(
        capture.source,
        FirLocalClassCaptureSource::ClassStorage {
            owner: outer.declaration,
            enclosing_depth: 0,
            field: 0,
        }
    );
}

#[test]
fn anonymous_initializer_capture_uses_its_constructor_parameter_without_storage() {
    let (body, _) = checked_function_body(
        "fun create(value: Int): Any = object { val value: Int = value }",
        "create",
    );
    let object = (0..body.expression_count())
        .find_map(
            |raw| match &body.expr(FirExprId::from_raw(raw as u32))?.kind {
                FirExprKind::AnonymousObject(object) => Some(object),
                _ => None,
            },
        )
        .expect("anonymous constructor plan");
    let [capture] = object.captures.as_ref() else {
        panic!("one constructor capture expected: {:?}", object.captures)
    };
    assert!(!capture.stored);
    assert_eq!(capture.ty.get(), Ty::Int);
    assert_eq!(
        capture.source,
        FirLocalClassCaptureSource::Value(body.parameters()[0].value)
    );
}

#[test]
fn anonymous_delegate_operands_follow_the_enclosing_construction_lifetime() {
    let source =
        include_str!("../../../../tests/box_regressions/interfaceDelegateCaptureLifetimes.kt");
    let (index, bodies) = checked_streamed_bodies(source);
    let mut constructor_owner = None;
    let mut later_owner = None;
    for (function, stored) in [("duringConstruction", false), ("afterConstruction", true)] {
        let selected = bodies
            .iter()
            .filter(|body| {
                index.declaration_name(DeclarationId::from_raw(body.owner().raw()))
                    == Some(function)
            })
            .collect::<Vec<_>>();
        let [body] = selected.as_slice() else {
            panic!("one checked body expected for {function}")
        };
        let objects = (0..body.expression_count())
            .filter_map(
                |raw| match &body.expr(FirExprId::from_raw(raw as u32))?.kind {
                    FirExprKind::AnonymousObject(object) => Some(object),
                    _ => None,
                },
            )
            .collect::<Vec<_>>();
        let [object] = objects.as_slice() else {
            panic!("one outer constructor plan expected: {objects:?}")
        };
        let [capture] = object.captures.as_ref() else {
            panic!("one enclosing value expected: {:?}", object.captures)
        };
        assert_eq!(capture.stored, stored, "{function}: capture lifetime");
        assert_eq!(capture.ty.get(), Ty::obj("Reader"));
        assert_eq!(
            capture.source,
            FirLocalClassCaptureSource::Value(body.parameters()[0].value)
        );
        if stored {
            later_owner = Some(object.declaration);
        } else {
            constructor_owner = Some(object.declaration);
        }
    }
    let constructor_owner = constructor_owner.expect("initializer retains its constructor owner");
    let later_owner = later_owner.expect("later invocation retains the outer storage owner");
    let delegates = bodies
        .iter()
        .flat_map(|body| {
            (0..body.expression_count()).filter_map(move |raw| {
                let FirExprKind::AnonymousObject(object) =
                    &body.expr(FirExprId::from_raw(raw as u32))?.kind
                else {
                    return None;
                };
                (object.delegate_arguments.len() == 1).then_some((body, object))
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(delegates.len(), 2);
    let mut constructor_reads = 0;
    let mut stored_reads = 0;
    for (body, object) in delegates {
        assert!(
            object.captures.is_empty(),
            "no phantom body capture: {object:?}"
        );
        let [argument] = object.delegate_arguments.as_ref() else {
            unreachable!("one delegate argument was selected")
        };
        assert_eq!(argument.delegation, 0);
        assert_eq!(
            index
                .classifier_header(object.declaration)
                .unwrap()
                .interface_delegations[0]
                .source,
            crate::fir::ResolvedInterfaceDelegateSource::SyntheticConstructorParameter(0)
        );
        match &body
            .expr(argument.value)
            .expect("checked delegate operand")
            .kind
        {
            FirExprKind::ConstructorCaptureRead {
                owner,
                field,
                shared_cell,
                site,
            } => {
                assert_eq!((*owner, *field), (constructor_owner, 0));
                assert!(!shared_cell);
                assert_eq!(*site, crate::fir::FirConstructorCaptureSite::Parameter);
                constructor_reads += 1;
            }
            FirExprKind::ClassStorageRead { owner, field } => {
                assert_eq!((*owner, *field), (later_owner, 0));
                stored_reads += 1;
            }
            other => panic!("delegate operand must retain its exact enclosing read: {other:?}"),
        }
    }
    assert_eq!((constructor_reads, stored_reads), (1, 1));
}

#[test]
fn initializer_function_value_reads_its_constructor_capture() {
    let (index, bodies) = checked_streamed_bodies(
        "fun create(read: String.() -> Int): Any = object { val result = \"word\".read() }",
    );
    let creators = bodies
        .iter()
        .filter(|body| {
            index.declaration_name(DeclarationId::from_raw(body.owner().raw())) == Some("create")
        })
        .collect::<Vec<_>>();
    let [creator] = creators.as_slice() else {
        panic!("one constructor call body expected")
    };
    let objects = (0..creator.expression_count())
        .filter_map(
            |raw| match &creator.expr(FirExprId::from_raw(raw as u32))?.kind {
                FirExprKind::AnonymousObject(object) => Some(object),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    let [object] = objects.as_slice() else {
        panic!("one anonymous constructor expected")
    };
    let [capture] = object.captures.as_ref() else {
        panic!("one function-value capture expected")
    };
    assert!(!capture.stored);
    assert_eq!(
        capture.source,
        FirLocalClassCaptureSource::Value(creator.parameters()[0].value)
    );
    let reads = bodies
        .iter()
        .flat_map(|body| {
            (0..body.expression_count()).filter_map(move |raw| {
                let FirExprKind::ConstructorCaptureRead {
                    owner, field, site, ..
                } = &body.expr(FirExprId::from_raw(raw as u32))?.kind
                else {
                    return None;
                };
                (*owner == object.declaration).then_some((*field, *site))
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reads,
        [(0, crate::fir::FirConstructorCaptureSite::Parameter)]
    );
}

#[test]
fn constructor_capture_families_project_one_canonical_field_through_shadowed_calls() {
    let source =
        include_str!("../../../../tests/box_regressions/captureStorageConstructorFamilies.kt");
    let (index, bodies) = checked_streamed_bodies(source);
    let selected = |name| {
        let mut selected = bodies.iter().filter(|body| {
            index.declaration_name(DeclarationId::from_raw(body.owner().raw())) == Some(name)
        });
        let body = selected.next().expect("one selected member body");
        assert!(selected.next().is_none());
        body
    };
    let check = selected("check");
    let captures = (0..check.statement_count())
        .filter_map(
            |raw| match &check.statement(FirStatementId::from_raw(raw as u32))?.kind {
                FirStatementKind::LocalDeclaration {
                    declaration,
                    captures,
                    ..
                } => Some((*declaration, captures)),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(captures.len(), 3);
    let mut identities = Vec::new();
    let mut a_owner = None;
    for (declaration, captures) in captures {
        let [capture] = captures.as_ref() else {
            panic!("each classifier stores exactly one resolved value: {captures:?}")
        };
        assert!(capture.stored);
        assert!(!capture.shared_cell);
        assert_eq!(capture.ty.get(), Ty::String);
        assert_eq!(
            capture.source,
            FirLocalClassCaptureSource::Value(check.parameters()[0].value)
        );
        let identity = capture.capture_identity.expect("canonical capture family");
        assert!(matches!(identity, ClassCaptureIdentity::Value { .. }));
        assert!(!identities.contains(&identity));
        identities.push(identity);
        if index.declaration_name(declaration) == Some("A") {
            a_owner = Some(declaration);
        }
    }
    let a_owner = a_owner.expect("declared subclass");
    let own = selected("own");
    let reads = (0..own.expression_count())
        .filter_map(
            |raw| match &own.expr(FirExprId::from_raw(raw as u32))?.kind {
                FirExprKind::ClassStorageRead { owner, field } => Some((*owner, *field)),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    assert_eq!(reads, [(a_owner, 0)]);
    for method in ["requestedB", "requestedC"] {
        let body = selected(method);
        let calls = (0..body.expression_count())
            .filter_map(|raw| {
                let FirExprKind::ConstructorCall(call) =
                    &body.expr(FirExprId::from_raw(raw as u32))?.kind
                else {
                    return None;
                };
                call.external_capture_arguments.as_deref()
            })
            .collect::<Vec<_>>();
        let [arguments] = calls.as_slice() else {
            panic!("one separately streamed constructor call: {calls:?}")
        };
        let [argument] = *arguments else {
            panic!("one projected capture operand: {arguments:?}")
        };
        assert!(!argument.shared_cell_holder);
        assert!(
            matches!(body.expr(argument.value).expect("selected operand").kind,
            FirExprKind::ClassStorageRead { owner, field: 0 } if owner == a_owner)
        );
    }
}
