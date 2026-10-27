//! Authoritative publication of stable source type-alias headers.

use super::*;

/// Publish every independent source alias in inventory order. A rejected alias remains a failed
/// stable declaration, but does not own or truncate any later alias's semantic publication.
pub(super) fn publish_all(
    headers: &crate::fir::StreamedHeaderModule,
    semantics: &ProductionSignatureSemantics<'_>,
    index: &mut crate::fir::ResolvedModuleIndex,
) -> Vec<crate::fir::DeclarationId> {
    headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == crate::fir::DeclarationKind::TypeAlias)
        .filter_map(|stub| {
            publish_type_alias(headers, semantics, index, stub)
                .err()
                .map(|()| stub.id)
        })
        .collect()
}

fn publish_type_alias(
    headers: &crate::fir::StreamedHeaderModule,
    semantics: &ProductionSignatureSemantics<'_>,
    index: &mut crate::fir::ResolvedModuleIndex,
    stub: &crate::fir::DeclarationStub,
) -> Result<(), ()> {
    let name = stub
        .lookup_name
        .and_then(|name| headers.lookup_names.get(name))
        .ok_or(())?;
    let anchor = headers
        .declarations
        .anchor(stub.id)
        .expect("a compact type-alias stub must retain its stable owner");
    let (owner, declaration_name) = match anchor.owner {
        Some(owner) => {
            let owner_header = index.declaration_header(owner).ok_or(())?;
            if owner_header
                .flags
                .has(crate::fir::DeclarationFlags::LOCAL_CLASS)
            {
                return Ok(());
            }
            let classifier = index.classifier_header(owner).ok_or(())?;
            let mut nested = Vec::new();
            let mut current = classifier.classifier;
            while let Some(parent) = current.nested_owner() {
                nested.push(current.nested_segment_ref());
                current = parent;
            }
            nested.push(current.segment_ref());
            nested.reverse();
            let mut semantic_owner = classifier.classifier.namespace();
            for segment in nested {
                semantic_owner = crate::types::type_name_child(semantic_owner, segment);
            }
            (
                semantic_owner,
                crate::types::ClassifierDeclarationName::Nested {
                    owner: classifier.classifier,
                    name: name.into(),
                },
            )
        }
        None => {
            let file_scope = headers.scopes.file(stub.source).ok_or(())?;
            let mut package = crate::types::TypeName::ROOT;
            for segment in headers.scopes.path(file_scope.package) {
                package = crate::types::type_name_child(
                    package,
                    headers.lookup_names.get(*segment).ok_or(())?,
                );
            }
            (
                package,
                crate::types::ClassifierDeclarationName::TopLevel {
                    package,
                    name: name.into(),
                },
            )
        }
    };
    let identity = crate::types::type_name_child(owner, name);
    let crate::fir::HeaderDeclarationKind::TypeAlias { target: syntax, .. } = headers
        .syntax
        .declaration(stub.id)
        .map(|declaration| declaration.kind)
        .ok_or(())?
    else {
        return Err(());
    };
    let scope = crate::fir::SignatureScope {
        owner: stub.id,
        source: stub.source,
    };
    let expansion = semantics
        .resolve_explicit_header_type(scope, syntax)
        .map_err(|_| ())?;
    let target = crate::libraries::type_alias_target_classifier(expansion).ok_or(())?;
    index
        .publish_type_alias_header(
            stub.id,
            identity,
            Some(declaration_name),
            target,
            expansion,
            crate::spelling::Spelled::default(),
        )
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use crate::libraries::{AliasExpansion, ClassifierDeclaration, EmptySymbolSource};
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::{type_name, ClassifierDeclarationName, Ty};

    #[test]
    fn alias_declaration_names_survive_source_header_and_provider_projection() {
        let source = "package alias_source_names\nclass Real\n\
            typealias `Top$Alias` = Real\n\
            class Host { typealias `Nested$Alias` = Real }\n";
        let mut features = crate::features::LangFeatures::new();
        features.enable("NestedTypeAliases");
        let mut diagnostics = crate::diag::DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features(
            &[crate::source::SourceInput::kotlin(source)],
            crate::frontend::PlatformProvider::jvm(Box::new(EmptySymbolSource)),
            &features,
            &mut diagnostics,
        );
        assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
        let index = analysis.streamed.as_ref().unwrap().module.index();
        let stable = crate::fir::StreamedModuleSymbols::for_file(index, 0);
        let legacy = crate::module_symbols::ModuleSymbols::new(&analysis.symbols);
        let package = type_name("alias_source_names");
        let owner = type_name("alias_source_names/Host");
        let target = type_name("alias_source_names/Real");
        for (namespace, name, identity, declaration_name) in [
            (
                SymbolNamespace::Package(package),
                "Top$Alias",
                type_name("alias_source_names/Top$Alias"),
                ClassifierDeclarationName::TopLevel {
                    package,
                    name: "Top$Alias".into(),
                },
            ),
            (
                SymbolNamespace::Classifier(owner),
                "Nested$Alias",
                type_name("alias_source_names/Host/Nested$Alias"),
                ClassifierDeclarationName::Nested {
                    owner,
                    name: "Nested$Alias".into(),
                },
            ),
        ] {
            let expected = AliasExpansion {
                identity,
                declaration_name: Some(std::sync::Arc::new(declaration_name.clone())),
                target,
                formals: Vec::new(),
                expansion: Ty::obj_name(target),
                expansion_spelling: Default::default(),
            };
            let header = index.type_alias_by_identity(identity).unwrap();
            assert_eq!(header.declaration_name, Some(declaration_name));
            assert_eq!(stable.type_alias_binding(identity), Some(expected.clone()));
            assert_eq!(legacy.type_alias_binding(identity), Some(expected.clone()));
            for provider in [&stable as &dyn SymbolSource, &legacy as &dyn SymbolSource] {
                assert_eq!(
                    provider.symbols(namespace, name).classifier_declaration,
                    Some(ClassifierDeclaration::TypeAlias(expected.clone()))
                );
            }
        }
    }

    #[test]
    fn an_unknown_alias_declaration_name_does_not_borrow_its_known_target_name() {
        let identity = type_name("alias_unknown_name/Alias");
        let target = type_name("alias_unknown_target/Real");
        let mut symbols = crate::frontend::FrontendSymbols::default();
        symbols.source_alias_fqns.insert(identity, target);
        symbols.source_alias_expansions.insert(
            identity,
            crate::libraries::TypeAliasTemplate {
                declaration_name: None,
                formals: Vec::new(),
                expansion: Ty::obj_name(target),
            },
        );
        let source = crate::module_symbols::ModuleSymbols::new(&symbols);
        assert_eq!(
            source.type_alias_binding(identity),
            Some(AliasExpansion {
                identity,
                declaration_name: None,
                target,
                formals: Vec::new(),
                expansion: Ty::obj_name(target),
                expansion_spelling: Default::default(),
            })
        );
    }
}
