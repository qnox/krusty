use super::*;
use crate::types::{type_name, ClassifierDeclarationName};

#[test]
fn declaration_name_projections_preserve_source_header_ownership() {
    let source = "package projection_source\nclass `Literal$Name`\n\
        class `Owner$Literal` { class `Child$Name` }\n";
    let mut diagnostics = crate::diag::DiagSink::new();
    let analysis = crate::frontend::analyze_source_set_with_features(
        &[crate::source::SourceInput::kotlin(source)],
        crate::frontend::PlatformProvider::jvm(Box::new(crate::libraries::EmptySymbolSource)),
        &crate::features::LangFeatures::new(),
        &mut diagnostics,
    );
    assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
    let index = analysis.streamed.as_ref().unwrap().module.index();
    let provider = StreamedModuleSymbols::for_file(index, 0);
    let package = type_name("projection_source");
    let owner = type_name("projection_source/Owner$Literal");
    for (identity, namespace, name, expected) in [
        (
            type_name("projection_source/Literal$Name"),
            SymbolNamespace::Package(package),
            "Literal$Name",
            ClassifierDeclarationName::TopLevel {
                package,
                name: "Literal$Name".into(),
            },
        ),
        (
            owner,
            SymbolNamespace::Package(package),
            "Owner$Literal",
            ClassifierDeclarationName::TopLevel {
                package,
                name: "Owner$Literal".into(),
            },
        ),
        (
            type_name("projection_source/Owner$Literal$Child$Name"),
            SymbolNamespace::Classifier(owner),
            "Child$Name",
            ClassifierDeclarationName::Nested {
                owner,
                name: "Child$Name".into(),
            },
        ),
    ] {
        let declaration = index.classifier_declaration(identity).unwrap();
        assert_eq!(
            index
                .classifier_header(declaration)
                .unwrap()
                .declaration_name,
            Some(expected.clone())
        );
        assert_eq!(
            provider
                .stable_classifier(identity)
                .unwrap()
                .declaration_name,
            Some(expected.clone())
        );
        assert_eq!(
            provider
                .symbols(namespace, name)
                .classifier
                .as_ref()
                .unwrap()
                .declaration_name,
            Some(expected)
        );
    }
}
