use super::*;

#[test]
fn local_classifier_header_resolves_statement_local_type_alias() {
    let source = r#"
abstract class A { abstract val p: String }
fun make(): A {
    typealias Text = String
    class B(override val p: Text) : A()
    return B("OK")
}
"#;
    let inputs = [SourceInput::kotlin(source).with_file_stem("LocalAliasHeader")];
    let mut diagnostics = DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis
        .streamed
        .as_ref()
        .expect("ordinary local declarations must not block Pass-1 finalization")
        .module
        .index();
    let classifier_declaration = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS))
                && index
                    .local_class_name_provenance(*declaration)
                    .is_some_and(|provenance| {
                        provenance
                            .segments
                            .last()
                            .is_some_and(|segment| segment == "B")
                    })
        })
        .expect("stable local B declaration");
    let classifier = index
        .classifier_header(classifier_declaration)
        .expect("checked local B classifier header");
    assert_eq!(
        index
            .declaration_name(classifier_declaration)
            .and_then(|name| name.rsplit('.').next()),
        Some("B"),
    );
    assert_eq!(
        index.classifier_identity(classifier_declaration),
        Some(classifier.classifier),
    );
    let constructor = (0..index.declaration_count())
        .map(|raw| crate::fir::DeclarationId::from_raw(raw as u32))
        .find(|declaration| {
            index
                .declaration_header(*declaration)
                .is_some_and(|header| {
                    header.owner == Some(classifier_declaration)
                        && header.kind == crate::fir::DeclarationKind::Constructor
                })
        })
        .expect("stable B constructor declaration");
    assert!(
        index.signature(constructor).is_none(),
        "an ordinary local constructor header is Pass-2 lexical work"
    );

    let census = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
    assert!(census.failures.is_empty(), "{:?}", census.failures);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
}
