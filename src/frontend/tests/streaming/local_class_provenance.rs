use super::*;

#[test]
fn source_set_records_anonymous_source_provenance_without_a_backend_name() {
    let source = "fun build(): Any = object {}";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Widget")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let declaration = *analysis.files[0]
        .anonymous_object_classes
        .values()
        .next()
        .expect("anonymous declaration");
    let crate::ast::Decl::Class(_) = analysis.files[0].decl(declaration) else {
        panic!("anonymous declaration was not a class");
    };
    assert_eq!(
        analysis.files[0].local_class_name_provenance[&declaration],
        crate::ast::LocalClassNameProvenance {
            lexical_owner: None,
            segments: vec!["build".to_string()],
            ordinal: Some(1),
        }
    );
    let enclosing = analysis.files[0]
        .anonymous_object_enclosing_functions
        .get(&declaration)
        .copied()
        .expect("anonymous enclosure identity");
    let crate::ast::AnonymousEnclosingFunction::TopLevel(function) = enclosing else {
        panic!("top-level build owner was not recorded exactly");
    };
    assert!(matches!(
        analysis.files[0].decl(function),
        crate::ast::Decl::Fun(function) if function.name == "build"
    ));
}

#[test]
fn stable_nested_classifier_provenance_keeps_exact_anonymous_ownership() {
    let source = "fun build(): Any = object { inner class Nested }";
    let inputs = [SourceInput::kotlin(source).with_file_stem("Widget")];
    let mut diagnostics = DiagSink::new();
    let analysis = analyze_source_set_with_features(
        &inputs,
        Box::new(EmptySymbolSource),
        &LangFeatures::new(),
        &mut diagnostics,
    );

    assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
    let nested_ast = analysis.files[0]
        .hoisted_classifier_source_names
        .iter()
        .find_map(|(declaration, source_name)| (source_name == "Nested").then_some(*declaration))
        .expect("nested anonymous member classifier");
    let crate::ast::Decl::Class(nested) = analysis.files[0].decl(nested_ast) else {
        panic!("nested declaration was not a classifier")
    };
    let index = analysis
        .streamed
        .as_ref()
        .expect("explicit public signature must finalize")
        .module
        .index();
    let declaration = stable_declaration_at(
        &analysis,
        0,
        nested.span,
        crate::fir::DeclarationKind::Classifier,
    );
    assert!(index
        .declaration_header(declaration)
        .expect("nested classifier header")
        .flags
        .has(crate::fir::DeclarationFlags::LOCAL_CLASS));
    let anonymous_ast = *analysis.files[0]
        .anonymous_object_classes
        .values()
        .next()
        .expect("anonymous declaration");
    let crate::ast::Decl::Class(anonymous_class) = analysis.files[0].decl(anonymous_ast) else {
        panic!("anonymous declaration was not a classifier")
    };
    let anonymous = stable_declaration_at(
        &analysis,
        0,
        anonymous_class.span,
        crate::fir::DeclarationKind::Classifier,
    );
    assert_eq!(
        index.local_class_name_provenance(declaration),
        Some(&crate::fir::LocalClassNameProvenance {
            source: crate::fir::SourceFileId::from_raw(0),
            lexical_owner: Some(anonymous),
            segments: vec!["Nested".to_string()].into_boxed_slice(),
            ordinal: None,
        })
    );
}
