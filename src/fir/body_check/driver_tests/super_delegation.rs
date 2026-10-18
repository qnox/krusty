use super::*;

#[test]
fn secondary_constructor_delegation_is_final_in_checked_fir() {
    let source = "class Built(val number: Int, val text: String = \"default\") {\n\
                      constructor(flag: Boolean = true) : this(text = \"chosen\", number = 1) { flag }\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Built")],
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let work = ordinary
        .into_iter()
        .find(|work| {
            work.kind == BodyKind::Constructor
                && streamed
                    .module
                    .index()
                    .declaration_anchor(work.declaration)
                    .is_some_and(|anchor| anchor.sibling == 1)
        })
        .expect("secondary constructor body unit");
    let default_owner = work.owner;
    let (index, mut inline_bodies, mut default_arguments, mut sources) =
        streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        crate::fir::BodyCheckUnit {
            file: &analysis.files[0],
            info: analysis.types[0].as_ref().expect("checked source"),
            source: SourceFileId::from_raw(0),
            index: &index,
        },
        work,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("secondary constructor must become checked FIR");

    assert_eq!(sink.0.len(), 1);
    let body = &sink.0[0].1;
    assert_eq!(body.parameters().len(), 1);
    assert!(body.default_values().is_empty());
    let defaults = default_arguments.take_for_source(&index, SourceFileId::from_raw(0));
    let default = defaults
        .iter()
        .find(|(_, body)| body.owner() == default_owner)
        .expect("secondary constructor default must be retained as Pass-1 FIR");
    assert_eq!(default.1.default_values().len(), 1);
    assert_eq!(body.roots().len(), 2);
    let delegation = body.statement(body.roots()[0]).expect("delegation root");
    let FirStatementKind::ConstructorDelegation(call) = &delegation.kind else {
        panic!("constructor delegation must be explicit FIR")
    };
    assert!(matches!(call.target, FirConstructorTarget::Module { .. }));
    assert_eq!(
        call.arguments
            .iter()
            .map(|argument| match argument {
                FirCallArgument::Expression { parameter, .. } => (*parameter, "value"),
                FirCallArgument::Default { parameter, .. } => (*parameter, "default"),
                FirCallArgument::Vararg { parameter, .. } => (*parameter, "vararg"),
            })
            .collect::<Vec<_>>(),
        [(1, "value"), (0, "value")]
    );
}

#[test]
fn secondary_constructor_val_initialization_is_a_checked_backing_field_write() {
    let source = "open class Base()\n\
                  class Built : Base {\n\
                      val stored: Any\n\
                      constructor(stored: Any) { this.stored = stored }\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("DeferredValConstructor")],
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let work = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .find(|work| {
            work.kind == BodyKind::Constructor
                && streamed
                    .module
                    .index()
                    .declaration_anchor(work.declaration)
                    .is_some_and(|anchor| anchor.sibling == 1)
        })
        .expect("secondary constructor body unit");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        crate::fir::BodyCheckUnit {
            file: &analysis.files[0],
            info: analysis.types[0].as_ref().expect("checked source"),
            source: SourceFileId::from_raw(0),
            index: &index,
        },
        work,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("secondary constructor must become checked FIR");

    let target = sink.0.iter().find_map(|(_, body)| {
        (0..body.expression_count()).find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            match expression.kind {
                FirExprKind::BackingFieldWrite { target, .. } => Some(target),
                _ => None,
            }
        })
    });
    let target = target.expect("deferred val initialization must target backing storage");
    assert_eq!(
        index
            .property(target)
            .and_then(|property| index.declaration_name(property.declaration)),
        Some("stored")
    );
}

#[test]
fn primary_super_delegation_uses_the_selected_module_constructor() {
    let source = "open class Base(val number: Int = 0, val text: String)\n\
                  class Child(val seed: Int = 7) : Base(text = \"chosen\", number = seed)\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Constructors")],
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let child = ordinary
        .into_iter()
        .find(|work| {
            work.kind == BodyKind::Constructor
                && streamed
                    .module
                    .index()
                    .declaration_anchor(work.declaration)
                    .is_some_and(|anchor| {
                        anchor.sibling == 0
                            && anchor.owner.is_some_and(|owner| {
                                streamed.module.index().declaration_name(owner) == Some("Child")
                            })
                    })
        })
        .expect("child primary constructor body unit");
    let default_owner = child.owner;
    let (index, mut inline_bodies, mut default_arguments, mut sources) =
        streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        crate::fir::BodyCheckUnit {
            file: &analysis.files[0],
            info: analysis.types[0].as_ref().expect("checked source"),
            source: SourceFileId::from_raw(0),
            index: &index,
        },
        child,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("primary constructor must become checked FIR");

    let body = &sink.0[0].1;
    assert_eq!(body.parameters().len(), 1);
    assert!(body.default_values().is_empty());
    let defaults = default_arguments.take_for_source(&index, SourceFileId::from_raw(0));
    let default = defaults
        .iter()
        .find(|(_, body)| body.owner() == default_owner)
        .expect("primary constructor default must be retained as Pass-1 FIR");
    assert_eq!(default.1.default_values().len(), 1);
    assert_eq!(body.roots().len(), 1);
    let FirStatementKind::ConstructorDelegation(call) = &body
        .statement(body.roots()[0])
        .expect("super delegation root")
        .kind
    else {
        panic!("primary super delegation must be explicit FIR")
    };
    assert!(matches!(call.target, FirConstructorTarget::Module { .. }));
    assert_eq!(
        call.arguments
            .iter()
            .map(|argument| match argument {
                FirCallArgument::Expression { parameter, .. } => *parameter,
                FirCallArgument::Default { parameter, .. } => *parameter,
                FirCallArgument::Vararg { parameter, .. } => *parameter,
            })
            .collect::<Vec<_>>(),
        [1, 0]
    );
}

#[test]
fn nested_class_super_argument_selects_its_enclosing_object_receiver() {
    let source = "open class Base(val value: String)\n\
                  object Host {\n\
                      class Derived constructor() : Base(this.value())\n\
                      fun value(): String = \"OK\"\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("ObjectHeaderReceiver")],
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let derived = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .find(|work| {
            work.kind == BodyKind::Constructor
                && streamed
                    .module
                    .index()
                    .declaration_anchor(work.declaration)
                    .is_some_and(|anchor| {
                        anchor.sibling == 0
                            && anchor.owner.is_some_and(|owner| {
                                streamed
                                    .module
                                    .index()
                                    .declaration_name(owner)
                                    .is_some_and(|name| name.ends_with(".Derived"))
                            })
                    })
        })
        .expect("Derived primary constructor body unit");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        crate::fir::BodyCheckUnit {
            file: &analysis.files[0],
            info: analysis.types[0].as_ref().expect("checked source"),
            source: SourceFileId::from_raw(0),
            index: &index,
        },
        derived,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("object receiver must become checked constructor FIR");

    let body = &sink.0[0].1;
    let FirStatementKind::ConstructorDelegation(delegation) = &body
        .statement(body.roots()[0])
        .expect("super delegation root")
        .kind
    else {
        panic!("primary super delegation must be explicit FIR")
    };
    let FirCallArgument::Expression { value, .. } = delegation.arguments[0] else {
        panic!("super argument must remain an expression")
    };
    let FirExprKind::Call(call) = &body.expr(value).expect("checked Host.value call").kind else {
        panic!("super argument must be a checked call")
    };
    let dispatch = call
        .dispatch_receiver
        .as_ref()
        .expect("Host object supplies the dispatch receiver");
    let dispatch = body
        .expr(dispatch.value)
        .expect("checked Host singleton receiver");
    assert!(
        matches!(
            &dispatch.kind,
            FirExprKind::SingletonValue { classifier, .. }
                if classifier.matches("Host")
        ),
        "{dispatch:?}"
    );
}

#[test]
fn primary_super_callable_reference_captures_the_available_enclosing_receiver() {
    let source = "abstract class Base(val fn: () -> String)\n\
                  class Outer {\n\
                      val ok = \"OK\"\n\
                      inner class Inner : Base(::ok)\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("OuterCapture")],
        crate::frontend::PlatformProvider::jvm(super::test_support::jvm_semantics()),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let inner = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .filter(|work| {
            work.kind == BodyKind::Constructor
                && streamed
                    .module
                    .index()
                    .declaration_anchor(work.declaration)
                    .is_some_and(|anchor| anchor.sibling == 0)
        })
        .max_by_key(|work| {
            streamed
                .module
                .index()
                .source_order(work.declaration)
                .expect("stable constructor order")
        })
        .expect("inner primary constructor body unit");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        crate::fir::BodyCheckUnit {
            file: &analysis.files[0],
            info: analysis.types[0].as_ref().expect("checked source"),
            source: SourceFileId::from_raw(0),
            index: &index,
        },
        inner,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("enclosing-receiver reference must become checked constructor FIR");

    let body = &sink.0[0].1;
    let FirStatementKind::ConstructorDelegation(call) = &body
        .statement(body.roots()[0])
        .expect("super delegation root")
        .kind
    else {
        panic!("primary super delegation must be explicit FIR")
    };
    let FirCallArgument::Expression { value, .. } = call.arguments[0] else {
        panic!("super constructor function argument must be an expression")
    };
    let FirExprKind::PropertyReference {
        binding: FirCallableReferenceBinding::Bound,
        dispatch_receiver: Some(receiver),
        ..
    } = &body.expr(value).expect("checked property reference").kind
    else {
        panic!("::ok must retain a bound stable property reference")
    };
    let receiver_expression = body
        .expr(receiver.value)
        .expect("checked enclosing receiver expression");
    assert!(matches!(
        &receiver_expression.kind,
        FirExprKind::EnclosingReceiver { path } if path.len() == 1
    ));
}

#[test]
fn inner_super_constructor_delegation_keeps_the_checked_enclosing_receiver() {
    let source = "open class Outer {\n\
                      open inner class Base\n\
                  }\n\
                  fun make() {\n\
                      val derived = object : Outer() {\n\
                          inner class Derived : Base()\n\
                      }\n\
                      derived.Derived()\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("InnerSuperOuter")],
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let derived = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .find(|work| {
            work.kind == BodyKind::Constructor
                && streamed
                    .module
                    .index()
                    .declaration_anchor(work.declaration)
                    .is_some_and(|anchor| {
                        anchor.sibling == 0
                            && anchor.owner.is_some_and(|owner| {
                                streamed
                                    .module
                                    .index()
                                    .declaration_name(owner)
                                    .is_some_and(|name| name.ends_with(".Derived"))
                            })
                    })
        })
        .expect("Derived primary constructor body unit");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        crate::fir::BodyCheckUnit {
            file: &analysis.files[0],
            info: analysis.types[0].as_ref().expect("checked source"),
            source: SourceFileId::from_raw(0),
            index: &index,
        },
        derived,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("inner-super receiver must become checked constructor FIR");

    let body = &sink.0[0].1;
    let FirStatementKind::ConstructorDelegation(call) = &body
        .statement(body.roots()[0])
        .expect("super delegation root")
        .kind
    else {
        panic!("primary super delegation must be explicit FIR")
    };
    let receiver = call
        .outer_receiver
        .expect("inner superclass constructor needs an enclosing receiver");
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::EnclosingReceiver { path }) if path.len() == 1
    ));
}

#[test]
fn anonymous_inner_super_delegation_reads_checked_constructor_capture_parameters() {
    let source = "fun box(): String {\n\
                      class Local {\n\
                          open inner class Inner(val value: String)\n\
                          val expected = \"OK\"\n\
                          val instance = object : Inner(expected) {}\n\
                      }\n\
                      return Local().instance.value\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("AnonymousInnerSuper")],
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let source_id = SourceFileId::from_raw(0);
    let active =
        ActiveSourceDeclarations::bind_complete_source(&analysis.files[0], source_id, &index)
            .expect("focused Pass 2 source binding");
    let root = index
        .declarations_named("box")
        .first()
        .copied()
        .expect("box declaration");
    let selected_roots = crate::fx_hash::HashSet::from_iter([active
        .span(&analysis.files[0], root)
        .expect("box source span")]);
    let selected_stable_bodies = ordinary
        .iter()
        .map(|work| work.declaration)
        .collect::<crate::fx_hash::HashSet<_>>();
    let mut selected_body_spans = selected_stable_bodies
        .iter()
        .filter_map(|declaration| active.span(&analysis.files[0], *declaration))
        .collect::<crate::fx_hash::HashSet<_>>();
    for &body in &selected_stable_bodies {
        let mut current = body;
        while let Some(owner) = index
            .declaration_header(current)
            .and_then(|header| header.owner)
            .or_else(|| {
                index
                    .declaration_anchor(current)
                    .and_then(|anchor| anchor.owner)
            })
        {
            if owner == root {
                if !index
                    .declaration_header(owner)
                    .is_some_and(|header| header.kind == DeclarationKind::Classifier)
                {
                    if let Some(span) = active.span(&analysis.files[0], owner) {
                        selected_body_spans.insert(span);
                    }
                }
                break;
            }
            if let Some(span) = active.span(&analysis.files[0], owner) {
                selected_body_spans.insert(span);
            }
            current = owner;
        }
    }
    let mut pass_two_symbols = std::mem::take(&mut analysis.symbols).into_pass_two_symbols();
    let cache = crate::fir::StreamedModuleProjectionCache::default();
    let info = crate::resolve::check_selected_declarations_in_pass_two(
        &analysis.files[0],
        0,
        &selected_roots,
        &selected_body_spans,
        &active,
        &selected_stable_bodies,
        &mut pass_two_symbols,
        &index,
        &cache,
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    crate::resolve::publish_checked_local_signatures_in_pass_two_root(
        &analysis.files[0],
        &active,
        source_id,
        pass_two_symbols.semantic_platform(),
        &info,
        &mut index,
        root,
        &selected_stable_bodies,
    )
    .expect("fresh Pass 2 local signatures");
    let mut sink = RecordingSink::default();
    let mut session = BodyCheckSession::default();
    for work in ordinary {
        check_and_dispatch_active_body_in_session(
            crate::fir::BodyCheckUnit {
                file: &analysis.files[0],
                info: &info,
                source: source_id,
                index: &index,
            },
            &active,
            work,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("every body must become checked FIR");
    }

    let (anonymous, body) = sink
        .0
        .iter()
        .find_map(|(owner, body)| {
            let declaration = DeclarationId::from_raw(owner.raw());
            let anchor = index.declaration_anchor(declaration)?;
            (anchor.kind == DeclarationKind::Constructor && anchor.sibling == 0)
                .then_some(anchor.owner)
                .flatten()
                .filter(|classifier| {
                    index
                        .declaration_header(*classifier)
                        .is_some_and(|header| header.flags.has(DeclarationFlags::ANONYMOUS_OBJECT))
                })
                .map(|classifier| (classifier, body))
        })
        .expect("anonymous primary constructor FIR");
    let FirStatementKind::ConstructorDelegation(call) = &body
        .statement(body.roots()[0])
        .expect("super delegation root")
        .kind
    else {
        panic!("anonymous primary constructor must carry explicit super delegation")
    };
    let outer = call
        .outer_receiver
        .expect("inner superclass outer receiver");
    assert!(matches!(
        body.expr(outer.value).map(|expression| &expression.kind),
        Some(FirExprKind::ConstructorCaptureRead {
            owner,
            field: 0,
            shared_cell: false,
            // Read in the constructor body itself: the prefix parameter is in scope.
            site: crate::fir::FirConstructorCaptureSite::Parameter,
        }) if *owner == anonymous
    ));
    assert_eq!(
        (0..body.expression_count())
            .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
            .filter(|expression| matches!(
                expression.kind,
                FirExprKind::ConstructorCaptureRead {
                    owner,
                    field: 0,
                    shared_cell: false,
                    site: crate::fir::FirConstructorCaptureSite::Parameter,
                } if owner == anonymous
            ))
            .count(),
        2,
        "both the implicit outer and the enclosing property receiver use the capture parameter",
    );
}
