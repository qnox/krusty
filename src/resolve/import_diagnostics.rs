//! Import-list validation over the same normalized provider records used by signature and body
//! resolution. Import diagnostics are file facts, so the module pass emits them once before
//! signature finalization; bounded body checking never reopens the import list.

use super::*;

/// Validate an import from left to right and return its single source diagnostic, if any.
fn import_path_diagnostic(
    import: &crate::ast::ImportPath,
    source: &dyn SymbolSource,
    resolver: &crate::symbol_resolver::SymbolResolver<'_>,
    access_package: TypeName,
) -> Option<(Span, String)> {
    use crate::libraries::Callables;
    use crate::symbol_source::SymbolNamespace;
    let unresolved =
        |segment: &str, span: Span| (span, format!("unresolved reference '{segment}'."));
    let mut prefix = ResolvedQualifier::Package(TypeName::ROOT);
    for (index, (segment, span)) in import.segments.iter().enumerate() {
        // A `.*` suffix means every stored segment is a qualifier, never the imported name.
        let terminal = index + 1 == import.segments.len() && !import.wildcard;
        match prefix {
            ResolvedQualifier::Value => return None,
            ResolvedQualifier::Package(package) => {
                let symbols = source.symbols(SymbolNamespace::Package(package), segment);
                if let Some(classifier) = symbols.classifier_name {
                    if terminal {
                        let shape = source.classifier(classifier)?;
                        if shape.access == crate::libraries::ClassifierAccess::PackagePrivate
                            && access_package != classifier.namespace()
                        {
                            return Some((
                                *span,
                                format!(
                                    "cannot access '{}': it is package-private in file.",
                                    classifier_access_display_from_shape(classifier, &shape),
                                ),
                            ));
                        }
                    }
                    prefix = ResolvedQualifier::Classifier(classifier);
                } else if source.package_exists(package, segment) {
                    prefix =
                        ResolvedQualifier::Package(crate::types::type_name_child(package, segment));
                } else if terminal
                    && (!matches!(symbols.callables, Callables::None)
                        || symbols.importable_declaration)
                {
                    return None;
                } else {
                    return Some(unresolved(segment, *span));
                }
            }
            ResolvedQualifier::Classifier(owner) => {
                let symbols = source.symbols(SymbolNamespace::Classifier(owner), segment);
                if let Some(classifier) = symbols.classifier_name {
                    prefix = ResolvedQualifier::Classifier(classifier);
                    continue;
                }
                if !terminal {
                    return Some(unresolved(segment, *span));
                }
                let inherited = crate::symbol_resolver::members_in_hierarchy(
                    source,
                    Ty::obj_name(owner),
                    segment,
                );
                let owner_type = source.classifier(owner);
                let imported_object_member =
                    crate::symbol_resolver::imported_object_member_symbols(source, owner, segment);
                let classifier_callable = resolver
                    .classifier_call_candidates(owner, segment)
                    .is_some_and(|(_, candidates)| !candidates.is_empty());
                if !matches!(symbols.callables, Callables::None)
                    || symbols.importable_declaration
                    || imported_object_member.is_some()
                    || classifier_callable
                    || resolver
                        .accessible_classifier_associated_property(owner, segment)
                        .is_some()
                    || owner_type
                        .as_ref()
                        .is_some_and(|owner| owner.is_enum_entry(segment))
                    || (owner_type.as_ref().is_some_and(|owner| owner.is_object())
                        && !matches!(inherited, Callables::None))
                {
                    return None;
                }
                let Some(owner_type) = owner_type else {
                    return Some(unresolved(segment, *span));
                };
                let instance_member = !matches!(inherited, Callables::None)
                    || owner_type
                        .members
                        .iter()
                        .any(|member| member.name == *segment);
                if instance_member {
                    return Some((
                        *span,
                        format!(
                            "cannot import '{segment}'. Functions and properties can only be imported from packages or objects."
                        ),
                    ));
                }
                return Some(unresolved(segment, *span));
            }
        }
    }
    None
}

fn reports(
    file: &File,
    module: &dyn SymbolSource,
    libraries: &dyn SemanticPlatform,
    resolver: &crate::symbol_resolver::SymbolResolver<'_>,
    access_package: TypeName,
) -> Vec<(Span, String)> {
    let source =
        crate::symbol_source::CompositeSource::new(vec![module, libraries as &dyn SymbolSource]);
    let mut reports = file
        .import_paths
        .iter()
        .filter_map(|import| import_path_diagnostic(import, &source, resolver, access_package))
        .collect::<Vec<_>>();

    // Classifiers occupy a single-symbol namespace: two distinct explicit imports under one
    // visible name are conflicting imports. Callable/property facets deliberately do not enter
    // this set; Kotlin unions those declarations and overload/value selection decides later.
    let mut classifiers_by_name = HashMap::<String, Vec<(TypeName, Option<TypeName>)>>::new();
    for import in &file.import_paths {
        let Some((visible, owner, declared_name)) = explicit_import_target(import, &source) else {
            continue;
        };
        let record = source.symbols(owner, &declared_name);
        let Some(classifier) = record.classifier_name else {
            continue;
        };
        let alias = match &record.classifier_declaration {
            Some(crate::libraries::ClassifierDeclaration::TypeAlias(alias)) => Some(alias.identity),
            Some(crate::libraries::ClassifierDeclaration::Ordinary(_)) | None => None,
        };
        let classifiers = classifiers_by_name.entry(visible).or_default();
        if !classifiers.contains(&(classifier, alias)) {
            classifiers.push((classifier, alias));
        }
    }
    let conflicting = classifiers_by_name
        .into_iter()
        .filter_map(|(name, classifiers)| (classifiers.len() > 1).then_some(name))
        .collect::<HashSet<_>>();
    for import in &file.import_paths {
        let Some((visible, owner, declared_name)) = explicit_import_target(import, &source) else {
            continue;
        };
        if !conflicting.contains(&visible)
            || source
                .symbols(owner, &declared_name)
                .classifier_name
                .is_none()
        {
            continue;
        }
        let Some((_, span)) = import.segments.last() else {
            continue;
        };
        reports.push((
            *span,
            format!("conflicting import: imported name '{visible}' is ambiguous."),
        ));
    }
    reports
}

fn emit(diags: &mut DiagSink, file: u32, reports: Vec<(Span, String)>) {
    diags.set_file(file);
    for (span, message) in reports {
        let identity = crate::diag::DiagnosticIdentity::ImportResolution { reference: span };
        let already_reported = diags
            .diags
            .iter()
            .any(|diagnostic| diagnostic.file == file && diagnostic.identity == Some(identity));
        if !already_reported {
            diags.error_with_identity(span, identity, message);
        }
    }
}

/// Validate every source file's import list while the complete module header provider is live.
/// This precedes signature finalization, so a failed signature cannot suppress file diagnostics.
pub(crate) fn check_module_import_paths(files: &[File], table: &SymbolTable, diags: &mut DiagSink) {
    for (file_index, file) in files.iter().enumerate() {
        let file_index = u32::try_from(file_index).expect("too many source files");
        let module = crate::module_symbols::ModuleSymbols::for_file(table, file_index);
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn SymbolSource,
            table.libraries.as_ref() as &dyn SymbolSource,
        ]);
        let scope = function_import_scope_with(
            file,
            table.libraries.platform_default_import_packages(),
            &source,
        );
        let package = source_package::identity(file.package.as_deref());
        let resolver = crate::symbol_resolver::SymbolResolver::new_import_scoped_with_module(
            table.libraries.as_ref(),
            &module,
            &scope,
        )
        .with_access_context(package, file_index, Vec::new());
        emit(
            diags,
            file_index,
            reports(file, &module, table.libraries.as_ref(), &resolver, package),
        );
    }
}
