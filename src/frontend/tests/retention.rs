use super::*;

fn production_module(source: &str) -> crate::fir::FrontendModule {
    let inputs = [SourceInput::kotlin(source).with_file_stem("Retention")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features_and_prepare(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        |_, _| {},
        &mut diagnostics,
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    analysis
        .streamed
        .expect("the inferred signature must finalize")
        .module
}

fn signature_graph_bytes(source: &str) -> usize {
    let inputs = [SourceInput::kotlin(source).with_file_stem("GraphRetention")];
    let mut diagnostics = DiagSink::new();
    let mut extractor = crate::fir::SignatureConstraintExtractor::default();
    let mut origins = crate::fir::OriginStore::default();
    let _headers = crate::fir::stream_file_stub_inventory(
        &inputs,
        &LangFeatures::new(),
        &mut diagnostics,
        |source, file, stubs| {
            extractor.extract_file(file, source, stubs, |span| origins.source(source, span));
        },
    );
    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    extractor.graph().storage_payload_bytes()
}

#[test]
fn temporary_signature_origins_do_not_enter_the_pass_two_source_map() {
    let mut expression = "100".to_string();
    for value in (0..100).rev() {
        expression = format!("if (flag) {value} else {expression}");
    }
    let large = production_module(&format!("fun choose(flag: Boolean) = {expression}\n"));

    assert!(large.inline_bodies().is_empty());
    assert!(large.default_arguments().is_empty());
    assert!(
        !large.index().retains_source_coordinates(),
        "Pass-1 declaration ranges must be destroyed before Pass 2"
    );
    assert!(
        large.sources().origins().is_empty(),
        "signature-expression coordinates must die with the temporary graph"
    );
}

#[test]
fn explicit_ordinary_body_growth_does_not_enter_the_signature_graph() {
    let source = |terms: usize| {
        let expression = std::iter::repeat_n("text", terms)
            .collect::<Vec<_>>()
            .join(" + ");
        format!(
            "fun work(): String {{\n\
                 val local = object {{ fun value(text: String) = {expression} }}\n\
                 return local.value(\"OK\")\n\
             }}\n"
        )
    };

    let small = signature_graph_bytes(&source(2));
    let large = signature_graph_bytes(&source(100));
    assert_eq!(
        large, small,
        "only inferred non-local signatures may contribute ordinary expression dependencies"
    );
}
