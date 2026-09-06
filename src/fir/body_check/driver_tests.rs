use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::fir::DeclarationFlags;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

use super::driver::check_and_dispatch_bound_body_in_session;
use super::test_support::root_expression;
use super::*;

#[derive(Default)]
struct RecordingSink(Vec<(BodyOwnerId, FirBody)>);

impl CheckedBodySink for RecordingSink {
    fn accept_finalized(&mut self, owner: BodyOwnerId, body: FirBody) {
        self.0.push((owner, body));
    }
}

#[test]
fn property_initializers_and_accessors_stream_as_distinct_body_units() {
    let source = "val initialized: Int = 1\n\
                  val computed: Int get() = 2\n\
                  var adjusted: Int = 0\n\
                      set(changed) { changed }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Properties")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    let mut sink = RecordingSink::default();
    let mut kinds = Vec::new();

    for work in ordinary {
        kinds.push(work.kind);
        check_and_dispatch_body(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("property body must become checked FIR");
    }

    assert_eq!(
        kinds,
        [
            BodyKind::Initializer,
            BodyKind::Getter,
            BodyKind::Initializer,
            BodyKind::Setter,
        ]
    );
    assert_eq!(sink.0.len(), 4);
    assert!(inline_bodies.is_empty());
    let setter = &sink.0[3].1;
    assert_eq!(setter.parameters().len(), 1);
    assert_eq!(setter.parameters()[0].ty, ResolvedTy::new(Ty::Int).unwrap());
}

#[test]
fn anonymous_property_initializer_reads_same_named_lexical_capture() {
    let source = "fun read(): Int {\n\
                      val value = 42\n\
                      val holder = object { val value: Int = value }\n\
                      return value\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("AnonymousPropertyCapture")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("anonymous signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("anonymous property initializer must become checked FIR");
    }

    let (owner, initializer) = sink
        .0
        .iter()
        .find_map(|(owner, body)| {
            let property = DeclarationId::from_raw(owner.raw());
            let classifier = index.declaration_anchor(property)?.owner?;
            (index.declaration_name(property) == Some("value")
                && index
                    .declaration_header(classifier)
                    .is_some_and(|header| header.flags.has(DeclarationFlags::ANONYMOUS_OBJECT)))
            .then_some((classifier, body))
        })
        .expect("anonymous value initializer FIR");
    assert!(matches!(
        initializer
            .expr(root_expression(initializer))
            .map(|expression| &expression.kind),
        Some(FirExprKind::ClassStorageRead {
            owner: selected,
            field: 0,
        }) if *selected == owner
    ));
}

#[test]
fn enum_entry_property_accessor_streams_as_checked_fir() {
    let source = "enum class Choice {\n\
                      ONLY { override val text: String get() = \"OK\" };\n\
                      abstract val text: String\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("EnumEntryAccessor")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    let mut sink = RecordingSink::default();
    let mut getter_bodies = Vec::new();
    for work in ordinary {
        let getter = work.kind == BodyKind::Getter;
        check_and_dispatch_body(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("enum-entry accessor must become checked FIR");
        if getter {
            getter_bodies.push(sink.0.last().expect("streamed getter").1.result_type());
        }
    }

    assert_eq!(getter_bodies, [Some(ResolvedTy::new(Ty::String).unwrap())]);
    assert!(inline_bodies.is_empty());
}

#[test]
fn local_classifier_property_reference_streams_with_captured_type_arguments() {
    let source = r#"// WITH_STDLIB
        import kotlin.reflect.KProperty1
        fun <T> genericFun(value: T): T {
            class Local(val item: T)
            val unwrapItem: KProperty1<Local, T> = Local::item
            return unwrapItem(Local(value))
        }
        fun box(): String = genericFun("OK")"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalPropertyReference")];
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
        crate::toolchain::classpath_jars_for(source),
    ));
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(classpath)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let local_items = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .filter(|declaration| {
            index.declaration_name(*declaration) == Some("item")
                && index
                    .declaration_header(*declaration)
                    .is_some_and(|header| {
                        header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    })
        })
        .collect::<Vec<_>>();
    assert!(
        !local_items.is_empty(),
        "local constructor property declaration"
    );
    assert!(
        local_items
            .iter()
            .all(|declaration| index.property_for_declaration(*declaration).is_some()),
        "every local constructor property coordinate must publish its FIR identity: {:?}",
        local_items
            .iter()
            .map(|declaration| (
                *declaration,
                index.declaration_anchor(*declaration),
                index.signature(*declaration),
                index.property_for_declaration(*declaration),
            ))
            .collect::<Vec<_>>(),
    );
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_body(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("the local classifier reference must become checked FIR");
    }

    assert_eq!(sink.0.len(), 3);
    assert!(inline_bodies.is_empty());
}

#[test]
fn script_body_streams_as_one_checked_body_unit() {
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin_script("val value = 1\nvalue + 2\n").with_file_stem("BuildScript")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked script");
    let mut sink = RecordingSink::default();
    let work = ordinary.into_iter().next().expect("script body work unit");
    assert_eq!(work.kind, BodyKind::Script);

    check_and_dispatch_body(
        &analysis.files[0],
        info,
        SourceFileId::from_raw(0),
        work,
        &index,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("script body must become checked FIR");

    assert_eq!(sink.0.len(), 1);
    assert!(inline_bodies.is_empty());
    let root = root_expression(&sink.0[0].1);
    assert!(matches!(
        sink.0[0].1.expr(root).map(|expression| &expression.kind),
        Some(FirExprKind::Block { .. })
    ));
}

#[test]
fn class_init_block_uses_its_stable_body_unit_anchor() {
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[
            SourceInput::kotlin("class Initialized(val seed: Int) { init { seed + 2 } }\n")
                .with_file_stem("Initialized"),
        ],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let work = ordinary
        .into_iter()
        .find(|work| work.kind == BodyKind::Initializer)
        .expect("class init body unit");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        work,
        &index,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("class init block must become checked FIR");

    assert_eq!(sink.0.len(), 1);
    assert_eq!(sink.0[0].0, work.owner);
    assert_eq!(sink.0[0].1.owner(), work.owner);
    assert_eq!(sink.0[0].1.parameters().len(), 1);
    assert_eq!(
        sink.0[0].1.parameters()[0].ty,
        ResolvedTy::new(Ty::Int).unwrap()
    );
}

#[test]
fn enum_init_block_uses_the_current_dispatch_receiver() {
    let source = "enum class Choice {\n\
                      FIRST;\n\
                      val label = \"OK\"\n\
                      init { label }\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("EnumInitReceiver")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let work = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .find(|work| {
            streamed
                .module
                .index()
                .declaration_anchor(work.declaration)
                .is_some_and(|anchor| anchor.kind == DeclarationKind::Initializer)
        })
        .expect("enum init body unit");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        work,
        &index,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("enum init block must become checked FIR");

    let body = &sink.0[0].1;
    let receivers = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::PropertyRead {
                dispatch_receiver: Some(receiver),
                ..
            } = &body.expr(FirExprId::from_raw(raw as u32))?.kind
            else {
                return None;
            };
            body.expr(receiver.value).map(|expression| &expression.kind)
        })
        .collect::<Vec<_>>();
    assert_eq!(receivers.len(), 1);
    assert!(matches!(
        receivers[0],
        FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        }
    ));
}

#[test]
fn enum_property_initializer_reads_a_prior_property_on_current_dispatch() {
    let source = "object Delegate {\n\
                      operator fun getValue(receiver: Any?, property: Any?): String = \"OK\"\n\
                  }\n\
                  enum class Choice {\n\
                      FIRST;\n\
                      val delegated by Delegate\n\
                      val result = delegated\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("EnumPropertyReceiver")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let work = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .find(|work| streamed.module.index().declaration_name(work.declaration) == Some("result"))
        .expect("enum property initializer body unit");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        work,
        &index,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("enum property initializer must become checked FIR");

    let body = &sink.0[0].1;
    let root = root_expression(body);
    let FirExprKind::PropertyRead {
        dispatch_receiver: Some(receiver),
        ..
    } = &body.expr(root).expect("initializer root").kind
    else {
        panic!("enum property initializer must be a checked property read")
    };
    assert!(matches!(
        body.expr(receiver.value).map(|expression| &expression.kind),
        Some(FirExprKind::ImplicitReceiver {
            current: true,
            depth: 0,
        })
    ));
}

#[test]
fn local_class_init_captures_the_nearer_constructor_parameter_value() {
    let source = "var result: String = \"Fail\"\n\
                  class A<T : String>(val value: T) {\n\
                      init {\n\
                          class B { init { result = value } }\n\
                          B()\n\
                      }\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("CapturedClassInit")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("every class initializer must become checked FIR");
    }

    assert!(sink.0.iter().any(|(_, body)| {
        (0..body.expression_count()).any(|raw| {
            matches!(
                body.expr(FirExprId::from_raw(raw as u32))
                    .map(|expression| &expression.kind),
                Some(FirExprKind::ClassStorageRead { field: 0, .. })
            )
        })
    }));
}

#[test]
fn local_class_member_keeps_the_enclosing_body_local_callable_identity() {
    let source = "fun box(): Int {\n\
                      val delta = 2\n\
                      infix fun Int.foo(value: Int): Int = value + delta\n\
                      val holder = object { fun test(): Int = 1 foo 1 }\n\
                      return holder.test()\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("LocalCallableCapture")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    let active = ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &index,
    )
    .expect("whole-file test syntax must bind before local signatures publish");
    let selected_root = ordinary
        .iter()
        .find(|work| {
            index
                .declaration_anchor(work.declaration)
                .is_some_and(|anchor| anchor.owner.is_none())
        })
        .map(|work| work.declaration)
        .expect("whole-file test work needs a root declaration");
    let selected_bodies = ordinary
        .iter()
        .map(|work| work.declaration)
        .collect::<std::collections::HashSet<_>>();
    crate::resolve::publish_checked_local_signatures_in_active_root(
        &analysis.files[0],
        &active,
        SourceFileId::from_raw(0),
        &analysis.symbols,
        info,
        &mut index,
        selected_root,
        &selected_bodies,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_active_body_in_session(
            &analysis.files[0],
            &active,
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("every local-class body must become checked FIR");
    }

    let declared = sink
        .0
        .iter()
        .flat_map(|(_, body)| {
            (0..body.statement_count()).filter_map(|raw| {
                let statement = body.statement(FirStatementId::from_raw(raw as u32))?;
                let FirStatementKind::LocalFunction { callable, .. } = statement.kind else {
                    return None;
                };
                Some(callable)
            })
        })
        .next()
        .expect("outer body local function declaration");
    let selected = sink
        .0
        .iter()
        .flat_map(|(_, body)| {
            (0..body.expression_count()).filter_map(|raw| {
                let expression = body.expr(FirExprId::from_raw(raw as u32))?;
                let FirExprKind::LocalCall { target, .. } = &expression.kind else {
                    return None;
                };
                let captures_are_checked_storage = target
                    .external_capture_arguments
                    .as_deref()
                    .is_some_and(|arguments| {
                        matches!(
                            arguments,
                            [argument]
                                if matches!(
                                    body.expr(*argument).map(|argument| &argument.kind),
                                    Some(FirExprKind::ClassStorageRead { field: 0, .. })
                                )
                        )
                    });
                Some((target.clone(), captures_are_checked_storage))
            })
        })
        .next()
        .expect("local-class method call to enclosing local function");
    assert_eq!(selected.0.callable, declared);
    assert_eq!(selected.0.body_depth, 1);
    assert!(
        selected.1,
        "the external local call must carry its exact class-storage closure operand"
    );
}

#[test]
fn anonymous_class_mutable_capture_is_a_shared_cell_in_checked_fir() {
    let source = "fun read(): Int {\n\
                      var value = 0\n\
                      val holder = object { fun current(): Int = value }\n\
                      value = 1\n\
                      return holder.current()\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("SharedClassCapture")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("mutable local-class capture must become checked FIR");
    }

    assert!(sink.0.iter().any(|(_, body)| {
        (0..body.expression_count()).any(|raw| {
            matches!(
                body.expr(FirExprId::from_raw(raw as u32))
                    .map(|expression| &expression.kind),
                Some(FirExprKind::AnonymousObject(object))
                    if object.captures.iter().any(|capture| capture.shared_cell)
            )
        })
    }));
    assert!(sink.0.iter().any(|(_, body)| {
        (0..body.expression_count()).any(|raw| {
            matches!(
                body.expr(FirExprId::from_raw(raw as u32))
                    .map(|expression| &expression.kind),
                Some(FirExprKind::ClassStorageSharedRead { field: 0, .. })
            )
        })
    }));
}

#[test]
fn anonymous_class_inferred_property_increment_keeps_outer_capture_mutable() {
    let source = "fun read(): Int {\n\
                      var value = 0\n\
                      val holder = object {\n\
                          val action = value++\n\
                      }\n\
                      holder.action\n\
                      return value\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("AnonymousLambdaCapture")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("anonymous accessor lambda must become checked FIR");
    }

    assert!(sink.0.iter().any(|(_, body)| {
        (0..body.expression_count()).any(|raw| {
            matches!(
                body.expr(FirExprId::from_raw(raw as u32))
                    .map(|expression| &expression.kind),
                Some(FirExprKind::AnonymousObject(object))
                    if object.captures.iter().any(|capture| {
                        capture.name.as_ref() == "value" && capture.shared_cell
                    })
            )
        })
    }));
    fn has_captured_class_storage(body: &FirBody, index: &ResolvedModuleIndex) -> bool {
        let selected = (0..body.expression_count()).any(|raw| {
            match body
                .expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind)
            {
                Some(
                    FirExprKind::ClassStorageSharedRead { .. }
                    | FirExprKind::ClassStorageSharedWrite { .. },
                ) => true,
                Some(
                    FirExprKind::CapturedClassStorageRead { owner, .. }
                    | FirExprKind::CapturedClassStorageSharedWrite { owner, .. },
                ) => {
                    body.implicit_receiver_captures()
                        .iter()
                        .any(|capture| capture.enclosing_depth == 0)
                        && index.classifier_header(*owner).is_some()
                }
                Some(FirExprKind::Lambda { body, .. }) => has_captured_class_storage(body, index),
                Some(_) | None => false,
            }
        });
        selected
    }
    assert!(sink
        .0
        .iter()
        .any(|(_, body)| has_captured_class_storage(body, &index)));
}

#[test]
fn nested_anonymous_class_forwards_a_mutable_destructure_capture() {
    let source = "// LANGUAGE: +NameBasedDestructuring\n\
                  class Parts {\n\
                      operator fun component1(): Int = 1\n\
                      operator fun component2(): Int = 2\n\
                  }\n\
                  fun exercise() {\n\
                      var [first, second] = Parts()\n\
                      val holder = object {\n\
                          fun update(next: Int) {\n\
                              val nested = object { fun apply() { first = next; first++ } }\n\
                              nested.apply()\n\
                          }\n\
                      }\n\
                      holder.update(second)\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let features = LangFeatures::from_source(source);
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("DestructureCapture")],
        Box::new(EmptySymbolSource),
        &features,
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("destructured mutable capture must become checked FIR");
    }

    assert!(sink.0.iter().any(|(_, body)| {
        (0..body.expression_count()).any(|raw| {
            matches!(
                body.expr(FirExprId::from_raw(raw as u32))
                    .map(|expression| &expression.kind),
                Some(FirExprKind::AnonymousObject(object))
                    if object.captures.iter().any(|capture| {
                            capture.name.as_ref() == "first" && capture.shared_cell
                        })
            )
        })
    }));
    assert!(sink.0.iter().any(|(_, body)| {
        (0..body.expression_count()).any(|raw| {
            matches!(
                body.expr(FirExprId::from_raw(raw as u32))
                    .map(|expression| &expression.kind),
                Some(FirExprKind::AnonymousObject(object))
                    if object.captures.iter().any(|capture| {
                        matches!(
                            &capture.source,
                            FirLocalClassCaptureSource::ClassStorage { .. }
                        )
                    })
            )
        })
    }));
    assert!(sink.0.iter().any(|(_, body)| {
        (0..body.expression_count()).any(|raw| {
            matches!(
                body.expr(FirExprId::from_raw(raw as u32))
                    .map(|expression| &expression.kind),
                Some(FirExprKind::ClassStorageSharedWrite { .. })
            )
        })
    }));
}

#[test]
fn inner_local_class_super_lambda_reads_the_enclosing_class_capture() {
    let source = "open class Base(val fn: () -> String)\n\
                  fun box(): String {\n\
                      val ok = \"OK\"\n\
                      class Local { inner class Inner : Base({ ok }) }\n\
                      return Local().Inner().fn()\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("NestedClassCapture")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("nested local-class bodies must become checked FIR");
    }

    fn has_enclosing_capture_read(body: &FirBody) -> bool {
        (0..body.expression_count()).any(|raw| {
            match body
                .expr(FirExprId::from_raw(raw as u32))
                .map(|expression| &expression.kind)
            {
                Some(FirExprKind::CapturedClassStorageRead {
                    field: 0,
                    shared_cell: false,
                    ..
                }) => true,
                Some(FirExprKind::Lambda { body, .. }) => has_enclosing_capture_read(body),
                Some(_) | None => false,
            }
        })
    }
    assert!(sink
        .0
        .iter()
        .any(|(_, body)| has_enclosing_capture_read(body)));
}

#[test]
fn local_class_inside_setter_keeps_enclosing_backing_field_identity() {
    let source = "class Host {\n\
                      var value: String = \"\"\n\
                          set(next) {\n\
                              class Local { fun write() { field = next } }\n\
                              Local().write()\n\
                          }\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("LocalSetterField")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("setter-local class bodies must become checked FIR");
    }

    let target = sink.0.iter().find_map(|(_, body)| {
        (0..body.expression_count()).find_map(|raw| {
            let expression = body.expr(FirExprId::from_raw(raw as u32))?;
            match expression.kind {
                FirExprKind::BackingFieldWrite { target, .. } => Some(target),
                _ => None,
            }
        })
    });
    let target = target.expect("local class method backing-field write");
    assert_eq!(
        index
            .property(target)
            .and_then(|property| index.declaration_name(property.declaration)),
        Some("value")
    );
}

#[test]
fn nested_anonymous_super_argument_retains_its_enclosing_instance_receiver() {
    let source = "open class X(val fn: () -> Unit)\n\
                  open class C(val x: X)\n\
                  class B(var value: Int) {\n\
                      fun update() {\n\
                          object : C(object : X({ value = 3 }) {}) {}\n\
                      }\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("NestedAnonymousReceiver")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut session = BodyCheckSession::default();
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
            &mut session,
        )
        .expect("nested anonymous-class bodies must become checked FIR");
    }

    let (dispatch_captures, forwarded_storage_captures) = sink
        .0
        .iter()
        .flat_map(|(_, body)| {
            (0..body.expression_count()).filter_map(|raw| {
                let expression = body.expr(FirExprId::from_raw(raw as u32))?;
                let FirExprKind::AnonymousObject(object) = &expression.kind else {
                    return None;
                };
                Some(
                    object
                        .captures
                        .iter()
                        .fold((0usize, 0usize), |counts, capture| match &capture.source {
                            FirLocalClassCaptureSource::DispatchReceiver => {
                                (counts.0 + 1, counts.1)
                            }
                            FirLocalClassCaptureSource::ClassStorage { .. }
                            | FirLocalClassCaptureSource::CapturedClassStorage { .. } => {
                                (counts.0, counts.1 + 1)
                            }
                            FirLocalClassCaptureSource::Value(_)
                            | FirLocalClassCaptureSource::Captured { .. }
                            | FirLocalClassCaptureSource::EnclosingReceiver { .. }
                            | FirLocalClassCaptureSource::CapturedImplicitReceiver { .. }
                            | FirLocalClassCaptureSource::ImplicitReceiver { .. } => counts,
                        }),
                )
            })
        })
        .fold((0usize, 0usize), |total, counts| {
            (total.0 + counts.0, total.1 + counts.1)
        });
    assert_eq!(dispatch_captures, 1);
    assert_eq!(forwarded_storage_captures, 1);

    fn captured_property_write_count(body: &FirBody, index: &ResolvedModuleIndex) -> usize {
        (0..body.expression_count())
            .map(|raw| {
                let Some(kind) = body
                    .expr(FirExprId::from_raw(raw as u32))
                    .map(|expression| &expression.kind)
                else {
                    return 0;
                };
                match kind {
                    FirExprKind::PropertyWrite {
                        dispatch_receiver: Some(receiver),
                        ..
                    } => {
                        let Some(FirExprKind::CapturedClassStorageRead {
                            owner,
                            receiver,
                            path,
                            field: 0,
                            shared_cell: false,
                        }) = body.expr(receiver.value).map(|expression| &expression.kind)
                        else {
                            return 0;
                        };
                        usize::from(
                            path.is_empty()
                                && index.declaration_header(*owner).is_some_and(|header| {
                                    header.flags.has(DeclarationFlags::ANONYMOUS_OBJECT)
                                })
                                && index
                                    .declaration_anchor(*owner)
                                    .and_then(|anchor| anchor.owner)
                                    .and_then(|owner| index.declaration_name(owner))
                                    == Some("update")
                                && matches!(
                                    body.expr(*receiver).map(|expression| &expression.kind),
                                    Some(FirExprKind::CapturedImplicitReceiver {
                                        enclosing_depth: 0,
                                        current: true,
                                        depth: 0,
                                        path,
                                    }) if path.is_empty()
                                ),
                        )
                    }
                    FirExprKind::Lambda { body, .. } => captured_property_write_count(body, index),
                    _ => 0,
                }
            })
            .sum()
    }
    assert_eq!(
        sink.0
            .iter()
            .map(|(_, body)| captured_property_write_count(body, &index))
            .sum::<usize>(),
        1,
    );
}

#[test]
fn class_initialization_units_stream_in_source_order() {
    let source = "class Ordered {\n\
                      val first: Int = 1\n\
                      init { 2 }\n\
                      val third: Int = 3\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Ordered")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let index = streamed.module.index();
    let class = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index.declaration_name(*declaration) == Some("Ordered")
                && index
                    .declaration_anchor(*declaration)
                    .is_some_and(|anchor| anchor.kind == DeclarationKind::Classifier)
        })
        .expect("stable class declaration");
    let mut initialization = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .filter_map(|declaration| {
            let anchor = index.declaration_anchor(declaration)?;
            (anchor.owner == Some(class)).then_some((
                index
                    .declaration_header(declaration)?
                    .initialization_order?,
                index
                    .declaration_name(declaration)
                    .unwrap_or("<init>")
                    .to_owned(),
            ))
        })
        .collect::<Vec<_>>();
    initialization.sort_by_key(|(order, _)| *order);
    assert_eq!(
        initialization,
        [
            (0, "first".to_owned()),
            (1, "<init>".to_owned()),
            (2, "third".to_owned())
        ]
    );

    let initialization_order = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .filter(|work| work.kind == BodyKind::Initializer)
        .map(|work| {
            streamed
                .module
                .index()
                .declaration_header(work.declaration)
                .and_then(|header| header.initialization_order)
                .expect("stable class initialization order")
        })
        .collect::<Vec<_>>();

    assert_eq!(initialization_order, [0, 1, 2]);
}

#[test]
fn enum_entry_constructor_mapping_is_final_in_checked_fir() {
    let source = "enum class Choice(val number: Int, val text: String = \"default\", vararg val flags: Int) {\n\
                      FIRST(text = \"chosen\", number = 1),\n\
                      SECOND(2)\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Choice")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let entries = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .filter(|work| work.kind == BodyKind::EnumEntry)
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    for work in entries {
        check_and_dispatch_body(
            &analysis.files[0],
            analysis.types[0].as_ref().expect("checked source"),
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("enum entry must become checked constructor FIR");
    }

    let arguments = sink
        .0
        .iter()
        .map(|(_, body)| {
            let root = root_expression(body);
            let FirExprKind::ConstructorCall(call) = &body.expr(root).unwrap().kind else {
                panic!("enum entry body must contain a selected constructor call")
            };
            call.arguments
                .iter()
                .map(|argument| match argument {
                    FirCallArgument::Expression { parameter, .. } => (*parameter, "value"),
                    FirCallArgument::Default { parameter, .. } => (*parameter, "default"),
                    FirCallArgument::Vararg { parameter, .. } => (*parameter, "vararg"),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(arguments[0], [(1, "value"), (0, "value"), (2, "vararg")]);
    assert_eq!(arguments[1], [(0, "value"), (1, "default"), (2, "vararg")]);
}

#[test]
fn enum_entry_property_initializers_stream_with_the_entry_receiver() {
    let source = r#"enum class Test(val x: String, val closure1: () -> String) {
        FOO("O", { FOO.x }) {
            override val y: String = "K"
            val closure = { y }
            override val z: String = closure()
        };
        abstract val y: String
        abstract val z: String
    }
"#;
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("EnumEntryProperties")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let property_initializers = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .filter(|work| {
            work.kind == BodyKind::Initializer
                && streamed
                    .module
                    .index()
                    .declaration_anchor(work.declaration)
                    .and_then(|anchor| anchor.owner)
                    .and_then(|owner| streamed.module.index().declaration_anchor(owner))
                    .is_some_and(|owner| owner.kind == DeclarationKind::EnumEntry)
        })
        .collect::<Vec<_>>();
    assert_eq!(property_initializers.len(), 3);

    let closure = (0..streamed.module.index().declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            streamed.module.index().declaration_name(*declaration) == Some("closure")
        })
        .and_then(|declaration| streamed.module.index().signature(declaration))
        .expect("enum-entry inferred property signature");
    assert_eq!(closure.result.get(), Ty::fun(Vec::new(), Ty::String));

    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    let mut sink = RecordingSink::default();
    for work in property_initializers {
        check_and_dispatch_body(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("enum-entry property initializer must become checked FIR");
    }
    assert_eq!(sink.0.len(), 3);

    let closure_body = sink
        .0
        .iter()
        .find(|(owner, _)| {
            index.declaration_name(DeclarationId::from_raw(owner.raw())) == Some("closure")
        })
        .map(|(_, body)| body)
        .expect("entry closure initializer body");
    let FirExprKind::Lambda {
        body: lambda_body, ..
    } = &closure_body
        .expr(root_expression(closure_body))
        .expect("entry closure initializer root")
        .kind
    else {
        panic!("entry closure initializer must retain its checked lambda")
    };
    let [receiver_capture] = lambda_body.implicit_receiver_captures() else {
        panic!("entry property read must capture exactly its entry receiver")
    };
    assert_eq!(receiver_capture.enclosing_depth, 0);
    assert!((0..lambda_body.expression_count()).any(|raw| {
        let Some(FirExprKind::CapturedClassStorageRead {
            owner, field: 0, ..
        }) = lambda_body
            .expr(FirExprId::from_raw(raw as u32))
            .map(|expression| &expression.kind)
        else {
            return false;
        };
        index
            .declaration_anchor(*owner)
            .is_some_and(|anchor| anchor.kind == DeclarationKind::EnumEntry)
    }));

    let base_y = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index.declaration_name(*declaration) == Some("y")
                && index
                    .declaration_anchor(*declaration)
                    .and_then(|anchor| anchor.owner)
                    .and_then(|owner| index.declaration_anchor(owner))
                    .is_some_and(|owner| owner.kind == DeclarationKind::Classifier)
        })
        .expect("enum abstract property");
    assert!(index
        .declaration_header(base_y)
        .expect("enum property header")
        .flags
        .has(DeclarationFlags::ABSTRACT));
}

#[test]
fn enum_entry_inner_class_reads_entry_property_through_outer_enum_receiver() {
    let source = r#"enum class A {
        X {
            val x = "OK"
            inner class Inner {
                inner class Inner2 {
                    inner class Inner3 { val y = x }
                }
            }
            val z = Inner().Inner2().Inner3()
            override val test: String get() = z.y
        };
        abstract val test: String
    }
    fun box() = A.X.test
"#;
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("EnumEntryInnerClass")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let x_property = (0..streamed.module.index().declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| streamed.module.index().declaration_name(*declaration) == Some("x"))
        .and_then(|declaration| {
            streamed
                .module
                .index()
                .property_for_declaration(declaration)
        })
        .expect("stable enum-entry property target");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let info = analysis.types[0].as_ref().expect("checked source");
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_body(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("enum-entry inner-class body must become checked FIR");
    }

    let receiver_paths = sink
        .0
        .iter()
        .flat_map(|(_, body)| {
            (0..body.expression_count()).filter_map(|raw| {
                let expression = body.expr(FirExprId::from_raw(raw as u32))?;
                let FirExprKind::PropertyRead {
                    target: FirPropertyTarget::Module(target),
                    dispatch_receiver: Some(receiver),
                    ..
                } = &expression.kind
                else {
                    return None;
                };
                (*target == x_property).then(|| {
                    let receiver = body.expr(receiver.value).expect("checked enum receiver");
                    let FirExprKind::EnclosingReceiver { path } = &receiver.kind else {
                        panic!("deep enum-entry read must retain its exact enclosing path")
                    };
                    (receiver.ty.get(), path.len())
                })
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(receiver_paths, [(Ty::obj("A"), 3)]);
}

#[test]
fn enum_entry_property_anonymous_object_captures_prior_entry_storage() {
    let source = r#"enum class X {
        B {
            val value2 = "K"
            val anonObject = object {
                override fun toString(): String = "O" + value2
            }
            override val value = anonObject.toString()
        };
        abstract val value: String
    }
"#;
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("EnumEntryAnonymousCapture")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    crate::resolve::publish_discovered_local_capture_declarations(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        &mut index,
    );
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &analysis.symbols,
        info,
        &mut index,
    )
    .expect("checked local signatures must publish before FIR body checking");
    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_body(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("enum-entry anonymous capture must become checked FIR");
    }
}

#[test]
fn anonymous_object_nested_inner_class_publishes_outer_capture_dependent_signatures() {
    let source = r#"class A(val x: String) {
        fun value(): String = object {
            inner class Y { val y = x }
            fun value() = Y().y
        }.value()
    }
"#;
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("AnonymousNestedInner")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    crate::resolve::publish_discovered_local_capture_declarations(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        &mut index,
    );
    let info = analysis.types[0].as_ref().expect("checked source");
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &analysis.symbols,
        info,
        &mut index,
    )
    .expect("anonymous nested-class signatures must publish");
    let y = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("y"))
        .and_then(|declaration| index.signature(declaration))
        .expect("stable local y signature");
    assert_eq!(y.result.get(), Ty::String);

    let mut sink = RecordingSink::default();
    for work in ordinary {
        check_and_dispatch_body(
            &analysis.files[0],
            info,
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("anonymous nested inner-class bodies must become checked FIR");
    }
}

#[test]
fn public_anonymous_result_call_uses_the_finalized_stable_approximation() {
    let source = r#"class Outer {
        class Nested {
            fun foo() = object {
                override fun toString() = "OK"
            }
        }
        fun test() = Nested().foo().toString()
    }
"#;
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &[SourceInput::kotlin(source).with_file_stem("PublicAnonymousResult")],
        super::test_support::jvm_semantics(),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let foo = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("foo"))
        .and_then(|declaration| index.signature(declaration))
        .expect("stable public foo signature");
    assert_eq!(foo.result.get(), Ty::obj("kotlin/Any"));

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
}

#[test]
fn production_stream_does_not_publish_anonymous_super_arguments_as_properties() {
    let source = "abstract class Base(val s: String, vararg ints: Int)\n\
                  fun foo(s: String, ints: IntArray) = object : Base(ints = *ints, s = s) {}\n\
                  fun box(): String {\n\
                      return foo(\"OK\", intArrayOf(1, 2)).s\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &[SourceInput::kotlin(source).with_file_stem("AnonymousSuperCapture")],
        super::test_support::jvm_semantics(),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn production_stream_republishes_anonymous_delegate_as_synthetic_constructor_parameter() {
    let source = "interface A { fun value(): String }\n\
                  class Impl : A { override fun value(): String = \"OK\" }\n\
                  fun box(impl: Impl): String {\n\
                      val suffix = \"!\"\n\
                      val delegated = object : A by impl { fun captured() = suffix }\n\
                      return delegated.value()\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &[SourceInput::kotlin(source).with_file_stem("AnonymousDelegate")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn production_stream_infers_nested_classifier_companion_invoke() {
    let source = "class A {\n\
                      class Nested {\n\
                          companion object { operator fun invoke(i: Int) = i }\n\
                      }\n\
                  }\n\
                  fun box() = if (A.Nested(42) == 42) \"OK\" else \"fail\"\n";
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &[SourceInput::kotlin(source).with_file_stem("NestedCompanionInvoke")],
        super::test_support::jvm_stdlib_semantics(),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn production_stream_finalizes_defaulted_member_through_interface_delegation() {
    let source = "interface A { fun foo(x: Int = 1): String }\n\
                  class B : A {\n\
                      override fun foo(x: Int): String = if (x == 1) \"OK\" else \"Fail\"\n\
                  }\n\
                  class X(val delegate: A = B()) : A by delegate\n";
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &[SourceInput::kotlin(source).with_file_stem("DelegatedDefault")],
        super::test_support::jvm_stdlib_semantics(),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert!(analysis.streamed.is_some(), "signatures must finalize");

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn production_stream_checks_expect_constructor_default_against_actual_signature() {
    let common = "expect class Value(text: String = \"OK\")\n";
    let platform = "actual class Value actual constructor(val text: String)\n\
                    fun box(): String = Value().text\n";
    let inputs = [
        SourceInput::kotlin(common)
            .with_file_stem("Common")
            .common(),
        SourceInput::kotlin(platform).with_file_stem("Platform"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::from_source("// LANGUAGE: +MultiPlatformProjects"),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn qualified_array_factory_is_folded_as_annotation_array_payload() {
    let source = r#"annotation class Entity(val foreignKeys: Array<String>)
        @Entity(foreignKeys = kotlin.arrayOf("id"))
        class Record
        fun box(): String = "OK"
    "#;
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &[SourceInput::kotlin(source).with_file_stem("QualifiedAnnotationArray")],
        super::test_support::jvm_semantics(),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn annotation_folding_uses_resolved_enum_identity_for_every_source_spelling() {
    let source = r#"// LANGUAGE: +ContextSensitiveResolutionUsingExpectedType
        package sample
        import sample.Choice.ONE
        enum class Choice { ONE, TWO }
        annotation class Marks(val first: Choice, val rest: Array<Choice>)
        annotation class Limit(val value: Int)
        @Marks(ONE, [TWO])
        fun marked() {}
        @Limit(MAX_VALUE)
        fun limited() {}
    "#;
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &[SourceInput::kotlin(source).with_file_stem("EnumAnnotationArguments")],
        super::test_support::jvm_semantics(),
        &LangFeatures::from_source(source),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn expected_classifier_resolves_a_java_static_property_as_a_semantic_property() {
    let kotlin = r#"// LANGUAGE: +ContextSensitiveResolutionUsingExpectedType
        fun selected(): JavaSingleton = JavaSingleton.consume(INSTANCE)
    "#;
    let inputs = [
        SourceInput::java(
            "public class JavaSingleton {\n\
                 public static JavaSingleton INSTANCE = new JavaSingleton();\n\
                 public static JavaSingleton consume(JavaSingleton value) { return value; }\n\
             }",
        )
        .with_file_stem("JavaSingleton"),
        SourceInput::kotlin(kotlin).with_file_stem("ExpectedJavaProperty"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        super::test_support::jvm_semantics(),
        &LangFeatures::from_source(kotlin),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn accessor_property_inherited_through_java_keeps_exact_source_accessor_targets() {
    let kotlin = r#"// LANGUAGE: -ForbidSyntheticPropertiesWithoutBaseJavaGetter
        abstract class Base(private var foo: String) {
            fun getFoo(): String = foo
            fun setFoo(value: String) { foo = value }
        }
        fun read(value: Intermediate): String = value.foo
        fun write(value: Intermediate) { value.foo = "K" }
    "#;
    let inputs = [
        SourceInput::java(
            "public class Intermediate extends Base {\n\
                 public Intermediate(String foo) { super(foo); }\n\
             }",
        )
        .with_file_stem("Intermediate"),
        SourceInput::kotlin(kotlin).with_file_stem("BaseAndUse"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        super::test_support::jvm_stdlib_semantics(),
        &LangFeatures::from_source(kotlin),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn inferred_signature_approximates_recursive_star_capture_to_denotable_type() {
    let source = r#"package a
        interface Rec<R, out T : Rec<R, T>> { fun t(): T }
        interface Super { fun foo(p: Rec<*, *>) = p.t() }
    "#;
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("RecursiveGeneric")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let index = analysis
        .streamed
        .as_ref()
        .expect("Pass 1 must finalize")
        .module
        .index();
    let result = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("foo"))
        .and_then(|declaration| index.signature(declaration))
        .map(|signature| signature.result.get())
        .expect("foo must have a finalized signature");
    let star = Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")));
    assert_eq!(
        result,
        Ty::obj_args(
            "a/Rec",
            &[
                star,
                Ty::star_projection(Ty::obj_args("a/Rec", &[star, star])),
            ],
        )
    );
}

#[test]
fn pass_one_prepares_inline_members_of_parser_hoisted_nested_classifiers() {
    let source = r#"open class Sized(val length: Int)
    class Outer {
        class Nested {
            inline fun <T : Sized> lengthOf(value: T) = value.length
            fun <T : Sized> ordinary(value: T) = value.length
        }
    }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("NestedInlinePreparation")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("nested inline member must be prepared during Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());
}

#[test]
fn pass_one_checks_anonymous_member_bodies_owned_by_an_inline_function() {
    let source = r#"interface Collector<T> { fun emit(value: T) }
        inline fun <T> collector(crossinline action: (T) -> Unit): Collector<T> =
            object : Collector<T> {
                override fun emit(value: T) = action(value)
            }
    "#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("InlineAnonymousBody")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );

    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let streamed = analysis
        .streamed
        .as_ref()
        .expect("inline-owned anonymous member signatures must be checked during Pass 1");
    assert_eq!(streamed.module.inline_bodies().len(), 1);
    let index = streamed.module.index();
    let emitted = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .filter(|declaration| index.declaration_name(*declaration) == Some("emit"))
        .filter_map(|declaration| index.signature(declaration))
        .map(|signature| signature.result.get())
        .collect::<Vec<_>>();
    assert_eq!(emitted, [Ty::Unit, Ty::Unit]);
    assert!(analysis.files.is_empty());
    assert!(analysis.types.is_empty());
}

#[test]
fn production_stream_captures_unnamed_context_in_crossinline_lambda() {
    let features = LangFeatures::from_source("// LANGUAGE: +ContextParameters\n");
    let inputs = [
        SourceInput::kotlin(
            "context(s: String) fun foo() = s\n\
             fun bar(f: () -> String) = f()\n\
             inline fun baz(crossinline f: () -> String) = bar { f() }\n",
        )
        .with_file_stem("Lib"),
        SourceInput::kotlin(
            "context(_: String) fun qux() = baz { foo() }\n\
             fun box() = context(\"OK\") { qux() }\n",
        )
        .with_file_stem("Main"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        super::test_support::jvm_stdlib_semantics(),
        &features,
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn production_stream_types_contextual_sam_lambda_with_implicit_receiver() {
    let features = LangFeatures::from_source("// LANGUAGE: +ContextParameters\n");
    let source = "open class A { fun foo(value: String): String = value }\n\
                  context(ctx: T) fun <T> implicit(): T = ctx\n\
                  fun interface Action { context(a: A) fun run(value: String): String }\n\
                  val action = Action { value: String -> implicit<A>().foo(value) }\n";
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &[SourceInput::kotlin(source).with_file_stem("ContextSam")],
        super::test_support::jvm_stdlib_semantics(),
        &features,
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn production_stream_publishes_cross_file_suspend_function_fun_interface() {
    let inputs = [
        SourceInput::kotlin("fun interface Foo : suspend () -> Unit\n").with_file_stem("Lib"),
        SourceInput::kotlin("val foo = Foo {}\nfun box(): String = \"OK\"\n")
            .with_file_stem("Main"),
    ];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        super::test_support::jvm_stdlib_semantics(),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.is_conformant(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn enum_entry_without_primary_constructor_targets_selected_secondary_callable() {
    let source = "enum class Test {\n\
                      A(0),\n\
                      B;\n\
                      val n: Int\n\
                      constructor(n: Int) { this.n = n }\n\
                      constructor() : this(0)\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("SecondaryEnum")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let entries = streamed
        .ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0))
        .into_iter()
        .filter(|work| work.kind == BodyKind::EnumEntry)
        .collect::<Vec<_>>();
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let class = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index.declaration_name(*declaration) == Some("Test")
                && index
                    .declaration_anchor(*declaration)
                    .is_some_and(|anchor| anchor.kind == DeclarationKind::Classifier)
        })
        .expect("enum declaration");
    let secondary_targets = [1, 2].map(|sibling| {
        (0..index.declaration_count())
            .map(|raw| DeclarationId::from_raw(raw as u32))
            .find_map(|declaration| {
                let anchor = index.declaration_anchor(declaration)?;
                (anchor.owner == Some(class)
                    && anchor.kind == DeclarationKind::Constructor
                    && anchor.sibling == sibling)
                    .then(|| index.callable_for_declaration(declaration))?
            })
            .expect("stable secondary constructor")
            .id
    });

    let mut sink = RecordingSink::default();
    for work in entries {
        check_and_dispatch_body(
            &analysis.files[0],
            analysis.types[0].as_ref().expect("checked source"),
            SourceFileId::from_raw(0),
            work,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("enum entry must consume the selected secondary constructor");
    }

    let targets = sink
        .0
        .iter()
        .map(|(_, body)| {
            let root = root_expression(body);
            let FirExprKind::ConstructorCall(call) = &body.expr(root).unwrap().kind else {
                panic!("enum entry body must contain a selected constructor call")
            };
            let FirConstructorTarget::Module(target) = call.target else {
                panic!("source enum entry must target a module constructor")
            };
            target
        })
        .collect::<Vec<_>>();
    assert_eq!(targets, secondary_targets);
}

#[test]
fn parameter_default_is_owned_by_fir_even_without_an_ordinary_body() {
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[
            SourceInput::kotlin("interface Config { fun value(input: Int = 41): Int }\n")
                .with_file_stem("Config"),
        ],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let work = ordinary
        .into_iter()
        .find(|work| work.kind == BodyKind::Function)
        .expect("default-expression body unit");
    let (index, mut inline_bodies, mut default_arguments, mut sources) =
        streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        work,
        &index,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("default expression must become checked FIR");

    assert!(
        sink.0.is_empty(),
        "a bodyless declaration has no Pass-2 FIR"
    );
    let defaults = default_arguments.take_for_source(&index, SourceFileId::from_raw(0));
    assert_eq!(defaults.len(), 1);
    let body = &defaults[0].1;
    assert!(body.roots().is_empty());
    assert_eq!(body.parameters().len(), 1);
    assert_eq!(body.default_values().len(), 1);
    assert_eq!(body.default_values()[0].parameter, 0);
    assert!(matches!(
        body.expr(body.default_values()[0].value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::Constant(FirConstant::Int(41)))
    ));
}

#[test]
fn member_constant_payload_survives_into_authoritative_pass_two() {
    let source = "var observed = 0\n\
                  object Config { const val VALUE = 42 }\n\
                  fun config(): Config { observed = 1; return Config }\n\
                  fun read(): Int = config().VALUE\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("ConstantReceiver")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let (index, _inline_bodies, _default_arguments, _sources) = streamed.module.into_parts();
    let constant = (0..index.declaration_count())
        .map(|raw| DeclarationId::from_raw(raw as u32))
        .find(|declaration| index.declaration_name(*declaration) == Some("VALUE"))
        .and_then(|declaration| index.compile_time_constant(declaration));
    assert!(
        constant.is_some(),
        "member const payload must survive Pass 1"
    );

    // The authoritative reparse must consume the stable constant payload. Runtime sequencing of
    // the ordinary value receiver is asserted by the exact end-to-end regression
    // `object_const_val_e2e::const_member_read_still_evaluates_its_value_receiver`.
    let inputs = [SourceInput::kotlin(source).with_file_stem("ConstantReceiver")];
    let production = crate::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    let census = crate::compiler::check_frontend_only(production, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}

#[test]
fn overloaded_functions_keep_parameter_referencing_defaults_in_their_own_signature_fragment() {
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(
            "fun choose(a: Int = 2, text: String = \"value\", c: Int = 4): String = text\n\
             fun choose(a: Int = 3, b: Int = a + 1, c: Int = a + b): Int = a + b + c\n",
        )
        .with_file_stem("Defaults")],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let work = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (index, mut inline_bodies, mut default_arguments, mut sources) =
        streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    for body in work {
        check_and_dispatch_body(
            &analysis.files[0],
            analysis.types[0].as_ref().expect("checked source"),
            SourceFileId::from_raw(0),
            body,
            &index,
            sources.origins_mut(),
            &mut inline_bodies,
            &mut sink,
        )
        .expect("every overload body must become checked FIR");
    }

    assert!(
        sink.0
            .iter()
            .all(|(_, body)| body.default_values().is_empty()),
        "ordinary Pass-2 bodies must not duplicate signature defaults"
    );
    let defaults = default_arguments.take_for_source(&index, SourceFileId::from_raw(0));

    let numeric = defaults
        .iter()
        .map(|(_, body)| body)
        .find(|body| {
            body.parameters().len() == 3
                && body
                    .parameters()
                    .iter()
                    .all(|parameter| parameter.ty.get() == Ty::Int)
        })
        .expect("the numeric overload must keep its own checked default fragment");
    assert_eq!(numeric.default_values().len(), 3);
    assert!(matches!(
        numeric
            .expr(numeric.default_values()[0].value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::Constant(FirConstant::Int(3)))
    ));
    assert!(matches!(
        numeric
            .expr(numeric.default_values()[1].value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::Binary { .. })
    ));
    assert!(matches!(
        numeric
            .expr(numeric.default_values()[2].value)
            .map(|expression| &expression.kind),
        Some(FirExprKind::Binary { .. })
    ));
}

#[test]
fn imported_nested_typealiases_publish_bound_constructions_and_unbound_constructor_reference() {
    let declarations = "// LANGUAGE: +NestedTypeAliases\n\
        package test\n\
        class Foo<T> {\n\
            inner class Inner(val value: T) { inner class Deeper<K>(val other: K) }\n\
            inner class Inner2<S>(val value: S)\n\
            typealias ToInner = Foo<String>.Inner\n\
            typealias ToInner2<S> = Foo<String>.Inner2<S>\n\
            typealias ToDeeper<K> = Foo<String>.Inner.Deeper<K>\n\
        }\n";
    let use_site = "// LANGUAGE: +NestedTypeAliases\n\
        package test\n\
        import test.Foo.ToInner\n\
        import test.Foo.ToInner2\n\
        import test.Foo.ToDeeper\n\
        fun box() {\n\
            val foo = Foo<String>()\n\
            val inner = foo.ToInner(\"OK\")\n\
            foo.ToInner2(42)\n\
            inner.ToDeeper('x')\n\
            val reference = Foo<String>::ToInner\n\
            reference(foo, \"OK\")\n\
        }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[
            SourceInput::kotlin(declarations).with_file_stem("Aliases"),
            SourceInput::kotlin(use_site).with_file_stem("UseSite"),
        ],
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let work = streamed
        .ordinary_body_work(&analysis.files[1], SourceFileId::from_raw(1))
        .into_iter()
        .find(|work| {
            work.kind == BodyKind::Function
                && streamed
                    .module
                    .index()
                    .declaration_anchor(work.declaration)
                    .is_some_and(|anchor| anchor.source.raw() == 1)
        })
        .expect("use-site function body");
    let (index, mut inline_bodies, _default_arguments, mut sources) = streamed.module.into_parts();
    let mut sink = RecordingSink::default();
    check_and_dispatch_body(
        &analysis.files[1],
        analysis.types[1].as_ref().expect("checked use site"),
        SourceFileId::from_raw(1),
        work,
        &index,
        sources.origins_mut(),
        &mut inline_bodies,
        &mut sink,
    )
    .expect("nested-alias use site must become checked FIR");

    let body = &sink.0[0].1;
    let bound_constructions = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter(|expression| {
            matches!(
                &expression.kind,
                FirExprKind::ConstructorCall(FirConstructorCall {
                    outer_receiver: Some(_),
                    ..
                })
            )
        })
        .count();
    assert_eq!(bound_constructions, 3);

    let constructor_reference = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find_map(|expression| match &expression.kind {
            FirExprKind::CallableReference {
                target:
                    FirCallableReferenceTarget::Constructor {
                        outer: Some(outer),
                        result,
                        ..
                    },
                function_type,
                binding: FirCallableReferenceBinding::Unbound,
                ..
            } => Some((*outer, *result, *function_type)),
            _ => None,
        })
        .expect("the alias constructor reference must keep checked constructor identity");
    assert_eq!(
        constructor_reference.0.get(),
        Ty::obj_args("test/Foo", &[Ty::String])
    );
    assert_eq!(
        constructor_reference.1.get().kotlin_class_internal(),
        Some(crate::types::type_name("test/Foo$Inner"))
    );
    assert_eq!(constructor_reference.1.get().type_args(), &[Ty::String]);
    let Ty::Fun(reference_shape) = constructor_reference.2.get() else {
        panic!("constructor reference must publish a function shape")
    };
    assert_eq!(reference_shape.params.len(), 2);
    assert_eq!(
        reference_shape.params[0],
        Ty::obj_args("test/Foo", &[Ty::String])
    );
}

#[test]
fn secondary_constructor_delegation_is_final_in_checked_fir() {
    let source = "class Built(val number: Int, val text: String = \"default\") {\n\
                      constructor(flag: Boolean = true) : this(text = \"chosen\", number = 1) { flag }\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("Built")],
        Box::new(EmptySymbolSource),
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
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        work,
        &index,
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
    assert!(matches!(call.target, FirConstructorTarget::Module(_)));
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
        Box::new(EmptySymbolSource),
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
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        work,
        &index,
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
        Box::new(EmptySymbolSource),
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
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        child,
        &index,
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
    assert!(matches!(call.target, FirConstructorTarget::Module(_)));
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
        Box::new(EmptySymbolSource),
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
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        derived,
        &index,
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
        super::test_support::jvm_semantics(),
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
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        inner,
        &index,
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
        Box::new(EmptySymbolSource),
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
        &analysis.files[0],
        analysis.types[0].as_ref().expect("checked source"),
        SourceFileId::from_raw(0),
        derived,
        &index,
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
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);

    let streamed = analysis.streamed.take().expect("Pass 1 must finalize");
    let ordinary = streamed.ordinary_body_work(&analysis.files[0], SourceFileId::from_raw(0));
    let (mut index, mut inline_bodies, _default_arguments, mut sources) =
        streamed.module.into_parts();
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        SourceFileId::from_raw(0),
        &mut analysis.symbols,
        analysis.types[0].as_ref().expect("checked source"),
        &mut index,
    )
    .expect("checked local signatures");
    let mut sink = RecordingSink::default();
    let mut session = BodyCheckSession::default();
    for work in ordinary {
        check_and_dispatch_bound_body_in_session(
            &analysis.files[0],
            analysis.types[0].as_ref().expect("checked source"),
            SourceFileId::from_raw(0),
            work,
            &index,
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
                .then(|| anchor.owner)
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
                } if owner == anonymous
            ))
            .count(),
        2,
        "both the implicit outer and the enclosing property receiver use the capture parameter",
    );
}
