use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::libraries::EmptySymbolSource;
use crate::source::SourceInput;

#[test]
fn deferred_local_constructor_signature_excludes_its_checked_body_capture_prefix() {
    let source = r#"
fun make(): Any {
    val captured = "OK"
    class Local() {
        fun read(): String = captured
    }
    return Local()
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("DeferredLocalCaptureConstructor")];
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);

    let parser_local = analysis.files[0]
        .local_class_enclosing_declarations
        .keys()
        .copied()
        .find(|declaration| {
            matches!(
                analysis.files[0].decl(*declaration),
                crate::ast::Decl::Class(class) if class.name.rsplit('.').next() == Some("Local")
            )
        })
        .expect("parser-hoisted local classifier");
    assert_eq!(
        analysis.types[0]
            .as_ref()
            .expect("Pass 2 checked local declarations")
            .local_class_captures_by_class
            .get(&parser_local)
            .map(Vec::len),
        Some(1),
        "the checked local declaration owns one physical capture prefix"
    );

    let source = crate::fir::SourceFileId::from_raw(0);
    let streamed = analysis
        .streamed
        .take()
        .expect("the module headers must finalize in Pass 1");
    assert!(!streamed.diagnostic_recovery);
    let (mut index, _, _, _) = streamed.module.into_parts();
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(
        &analysis.files[0],
        source,
        &index,
    )
    .expect("local declarations bind to their stable inventory");
    let classifier = active
        .canonical_classifier_declaration(parser_local, &index)
        .expect("stable local classifier declaration");
    let constructor = index
        .owned_declaration(classifier, crate::fir::DeclarationKind::Constructor, 0)
        .expect("stable primary constructor declaration");
    assert!(
        index
            .signature(constructor)
            .expect("the complete source constructor ABI publishes in Pass 1")
            .parameters
            .is_empty(),
        "Pass 1 source signatures exclude the later checked-FIR physical capture prefix"
    );

    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        source,
        &analysis.symbols,
        analysis.types[0]
            .as_ref()
            .expect("Pass 2 checked local declarations"),
        &mut index,
    )
    .expect("deferred local signatures publish from checked facts");

    assert!(
        index
            .signature(constructor)
            .expect("finalized primary constructor signature")
            .parameters
            .is_empty(),
        "stable source signatures exclude the checked-FIR physical capture prefix"
    );
    let callable = index
        .callable_for_declaration(constructor)
        .expect("finalized primary constructor callable");
    assert_eq!(
        index.callable_parameter_name_count(callable.id),
        0,
        "stable callable parameters describe source/context parameters only"
    );
}
