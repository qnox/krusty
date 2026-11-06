use super::*;

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
