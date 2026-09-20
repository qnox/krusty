//! Resolver-owned classifier bindings consumed by compact expect/actual matching.
//!
//! Header actualization runs before full signature solving because it decides which declaration
//! subtree survives. It still must not interpret imports or manufacture identities from spelling.
//! This module runs the ordinary provider-backed classifier scope tower once and publishes only
//! stable identities back to the compact header phase.

use super::*;

struct HeaderModuleClassifiers<'a> {
    declarations: &'a std::collections::HashSet<TypeName>,
}

impl SymbolSource for HeaderModuleClassifiers<'_> {
    fn symbols(
        &self,
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        let candidate = namespace
            .existing_classifier(name)
            .filter(|candidate| self.declarations.contains(candidate));
        candidate.map_or_else(
            || std::rc::Rc::new(crate::libraries::ResolvedSymbols::default()),
            |classifier| {
                std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                    classifier_name: Some(classifier),
                    classifier: Some(std::sync::Arc::new(
                        crate::libraries::LibraryType::declaration_header(),
                    )),
                    callables: crate::libraries::Callables::default(),
                    importable_declaration: true,
                })
            },
        )
    }

    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        let Some(package) = crate::types::existing_type_name_child(parent, name) else {
            return false;
        };
        self.declarations.iter().any(|declaration| {
            let mut current = declaration.parent();
            while let Some(owner) = current {
                if owner == package {
                    return true;
                }
                current = owner.parent();
            }
            false
        })
    }
}

struct ResolverInputs<'a> {
    module: HeaderModuleClassifiers<'a>,
    scopes: Vec<Option<crate::symbol_resolver::FunctionImportScope>>,
    platform: &'a dyn SemanticPlatform,
}

impl ResolverInputs<'_> {
    fn resolve(&self, source: crate::fir::SourceFileId, spelling: &str) -> Option<TypeName> {
        let scope = self.scopes.get(source.raw() as usize)?.as_ref()?;
        let resolver = crate::symbol_resolver::SymbolResolver::new_import_scoped_with_module(
            self.platform,
            &self.module,
            scope,
        );
        match resolver.qualified_classifier_binding_in_scope(spelling).0 {
            crate::symbol_resolver::CandidateSelection::Selected(classifier) => Some(classifier),
            crate::symbol_resolver::CandidateSelection::None
            | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
        }
    }
}

fn resolver_inputs<'a>(
    headers: &crate::fir::StreamedHeaderModule,
    platform: &'a dyn SemanticPlatform,
    declarations: &'a std::collections::HashSet<TypeName>,
) -> ResolverInputs<'a> {
    let module = HeaderModuleClassifiers { declarations };
    let source = crate::symbol_source::CompositeSource::new(vec![
        &module as &dyn SymbolSource,
        platform as &dyn SymbolSource,
    ]);
    let scopes = (0..headers.sources.len())
        .map(|index| {
            let source_id = crate::fir::SourceFileId::from_raw(index as u32);
            headers.scopes.file(source_id)?;
            let imports = compact_source_imports(headers, source_id)?;
            let mut explicit_targets = std::collections::HashMap::<
                String,
                Option<(crate::symbol_source::SymbolNamespace, String)>,
            >::new();
            let mut stars = Vec::new();
            for import in imports {
                if import.wildcard {
                    let owner = match qualifier_path(&import.path, &source, None).ok()? {
                        ResolvedQualifier::Package(package) => package,
                        ResolvedQualifier::Classifier(classifier) => classifier,
                        ResolvedQualifier::Value => return None,
                    };
                    if !stars.contains(&owner) {
                        stars.push(owner);
                    }
                    continue;
                }
                let (parent, declared_name) = import
                    .path
                    .rsplit_once('.')
                    .unwrap_or(("", import.path.as_str()));
                let owner = if parent.is_empty() {
                    crate::symbol_source::SymbolNamespace::Package(TypeName::ROOT)
                } else {
                    match qualifier_path(parent, &source, None).ok()? {
                        ResolvedQualifier::Package(package) => {
                            crate::symbol_source::SymbolNamespace::Package(package)
                        }
                        ResolvedQualifier::Classifier(classifier) => {
                            crate::symbol_source::SymbolNamespace::Classifier(classifier)
                        }
                        ResolvedQualifier::Value => return None,
                    }
                };
                let target = (owner, declared_name.to_owned());
                match explicit_targets.entry(import.visible_name) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(Some(target));
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if entry.get().as_ref() != Some(&target) {
                            entry.insert(None);
                        }
                    }
                }
            }
            let ambiguous = explicit_targets
                .iter()
                .filter_map(|(name, target)| target.is_none().then_some(name.clone()))
                .collect();
            let explicit = explicit_targets
                .into_iter()
                .filter_map(|(name, target)| {
                    let (owner, declared_name) = target?;
                    Some((
                        name,
                        crate::symbol_resolver::CallableImport::new(owner, declared_name),
                    ))
                })
                .collect();
            let own_package = headers.sources.get(source_id)?.package;
            let kotlin_defaults = KOTLIN_DEFAULT_IMPORT_PACKAGES
                .iter()
                .map(|package| crate::types::type_name(&package.replace('.', "/")))
                .collect();
            let platform_defaults = platform
                .platform_default_import_packages()
                .into_iter()
                .map(|package| crate::types::type_name(&package.replace('.', "/")))
                .collect();
            Some(
                crate::symbol_resolver::FunctionImportScope::new(
                    explicit,
                    [vec![own_package], stars, kotlin_defaults, platform_defaults],
                )
                .with_ambiguous_explicit(ambiguous),
            )
        })
        .collect();
    ResolverInputs {
        module,
        scopes,
        platform,
    }
}

fn bind_type(
    headers: &crate::fir::StreamedHeaderModule,
    resolver: &ResolverInputs<'_>,
    bindings: &mut crate::fir::ActualizationTypeBindings,
    source: crate::fir::SourceFileId,
    syntax: crate::fir::HeaderTypeId,
    visited: &mut std::collections::HashSet<(crate::fir::SourceFileId, crate::fir::HeaderTypeId)>,
) {
    if !visited.insert((source, syntax)) {
        return;
    }
    let Some(ty) = headers.syntax.ty(syntax) else {
        return;
    };
    match ty.kind {
        crate::fir::HeaderTypeKind::Classifier {
            detail,
            abbreviated_argument,
        } => {
            if let Some(detail) = headers.syntax.classifier_type(detail) {
                let spelling = headers
                    .syntax
                    .type_path(detail.path)
                    .iter()
                    .filter_map(|segment| headers.lookup_names.get(*segment))
                    .collect::<Vec<_>>()
                    .join(".");
                if let Some(classifier) = resolver.resolve(source, &spelling) {
                    bindings.bind_type(source, syntax, classifier);
                }
                for &argument in headers.syntax.type_operands(detail.arguments) {
                    bind_type(headers, resolver, bindings, source, argument, visited);
                }
            }
            if let Some(argument) = abbreviated_argument {
                bind_type(headers, resolver, bindings, source, argument, visited);
            }
        }
        crate::fir::HeaderTypeKind::Function {
            parameters, result, ..
        } => {
            for &parameter in headers.syntax.type_operands(parameters) {
                bind_type(headers, resolver, bindings, source, parameter, visited);
            }
            if let Some(result) = result {
                bind_type(headers, resolver, bindings, source, result, visited);
            }
        }
    }
}

pub(crate) fn actualization_type_bindings(
    headers: &crate::fir::StreamedHeaderModule,
    platform: &dyn SemanticPlatform,
) -> crate::fir::ActualizationTypeBindings {
    let declarations = headers
        .source_classifier_names()
        .into_iter()
        .chain(headers.stubs.iter().filter_map(|stub| {
            (stub.kind == crate::fir::DeclarationKind::TypeAlias)
                .then(|| compact_classifier_identity(headers, stub).map(|(_, identity)| identity))
                .flatten()
        }))
        .collect::<std::collections::HashSet<_>>();
    let resolver = resolver_inputs(headers, platform, &declarations);
    let mut bindings = crate::fir::ActualizationTypeBindings::default();
    let mut visited = std::collections::HashSet::new();
    for stub in &headers.stubs {
        if matches!(
            stub.kind,
            crate::fir::DeclarationKind::Classifier | crate::fir::DeclarationKind::TypeAlias
        ) {
            if let Some((_, identity)) = compact_classifier_identity(headers, stub) {
                bindings.bind_declaration(stub.id, identity);
            }
        }
        for syntax in headers.syntax.declaration_type_roots(stub.id) {
            bind_type(
                headers,
                &resolver,
                &mut bindings,
                stub.source,
                syntax,
                &mut visited,
            );
        }
    }
    bindings
}

#[cfg(test)]
pub(crate) fn resolve_actualization_classifier_for_test(
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
    spelling: &str,
) -> Option<TypeName> {
    let declarations = headers
        .source_classifier_names()
        .into_iter()
        .chain(headers.stubs.iter().filter_map(|stub| {
            (stub.kind == crate::fir::DeclarationKind::TypeAlias)
                .then(|| compact_classifier_identity(headers, stub).map(|(_, identity)| identity))
                .flatten()
        }))
        .collect::<std::collections::HashSet<_>>();
    let platform = crate::libraries::EmptySymbolSource;
    resolver_inputs(headers, &platform, &declarations).resolve(source, spelling)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DependencyClassifiers(std::collections::HashSet<TypeName>);

    impl SymbolSource for DependencyClassifiers {
        fn symbols(
            &self,
            namespace: crate::symbol_source::SymbolNamespace,
            name: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            let candidate = namespace
                .existing_classifier(name)
                .filter(|candidate| self.0.contains(candidate));
            candidate.map_or_else(
                || std::rc::Rc::new(crate::libraries::ResolvedSymbols::default()),
                |classifier| {
                    std::rc::Rc::new(crate::libraries::ResolvedSymbols {
                        classifier_name: Some(classifier),
                        classifier: Some(std::sync::Arc::new(
                            crate::libraries::LibraryType::declaration_header(),
                        )),
                        callables: crate::libraries::Callables::default(),
                        importable_declaration: true,
                    })
                },
            )
        }

        fn package_exists(&self, parent: TypeName, name: &str) -> bool {
            let Some(package) = crate::types::existing_type_name_child(parent, name) else {
                return false;
            };
            self.0.iter().any(|declaration| {
                let mut current = declaration.parent();
                while let Some(owner) = current {
                    if owner == package {
                        return true;
                    }
                    current = owner.parent();
                }
                false
            })
        }
    }

    impl SemanticPlatform for DependencyClassifiers {}

    fn headers(platform_import: &str) -> crate::fir::StreamedHeaderModule {
        let platform_source = format!(
            "// LANGUAGE: +MultiPlatformProjects\n\
             package fixture\n\
             import {platform_import}.*\n\
             actual fun consume(value: Marker): Int = 1\n"
        );
        let texts = [
            "// LANGUAGE: +MultiPlatformProjects\n\
             package fixture\n\
             import dependency.left.*\n\
             expect fun consume(value: Marker): Int\n",
            platform_source.as_str(),
        ];
        let inputs = texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                crate::source::SourceInput::kotlin(text).with_file_stem(if index == 0 {
                    "Common"
                } else {
                    "Platform"
                })
            })
            .collect::<Vec<_>>();
        let mut diagnostics = crate::diag::DiagSink::new();
        let files = texts
            .iter()
            .map(|text| {
                crate::frontend::parse_source_with_detected_features(text, &mut diagnostics)
            })
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
        crate::fir::inventory_parsed_source_headers(&inputs, &files)
    }

    #[test]
    fn dependency_wildcards_bind_through_the_provider_before_actualization() {
        let platform = DependencyClassifiers(
            [
                crate::types::type_name("dependency/left/Marker"),
                crate::types::type_name("dependency/right/Marker"),
                crate::types::type_name("kotlin/Int"),
            ]
            .into_iter()
            .collect(),
        );

        let same = headers("dependency.left");
        let bindings = actualization_type_bindings(&same, &platform);
        assert_eq!(crate::fir::actualization(&same, &bindings).pairs.len(), 1);

        let different = headers("dependency.right");
        let bindings = actualization_type_bindings(&different, &platform);
        let actualization = crate::fir::actualization(&different, &bindings);
        assert!(actualization.pairs.is_empty());
        assert_eq!(actualization.incompatible.len(), 1);
    }
}
