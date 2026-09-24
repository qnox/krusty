//! Projection of compact Pass-1 headers into the facts signature collection reads directly.
//!
//! Packages, imports, type-alias expansion, lexical classifier names, and annotation retention and
//! target policy all exist in the compact header module. This module reads them there rather than
//! re-walking the transient parse, and disappears with the rest of the legacy publisher.

use super::*;

pub(in crate::resolve) fn resolve_source_alias_expansion(
    target: &TypeRef,
    formals: &[String],
    visible_aliases: &[(String, Vec<String>, TypeRef)],
    names: &ClassNames,
    known_spellings: &HashMap<TypeName, (crate::spelling::Spelled, Vec<String>, Ty)>,
    diags: &mut DiagSink,
) -> Option<(Ty, crate::spelling::Spelled)> {
    let symbolic =
        TParams::symbolic_from_decl_with(formals, &[], &|candidate| names.get_class(candidate));
    let mut target_spellings = HashMap::new();
    let expanded_target =
        crate::parser::expanded_type_alias_target(visible_aliases, target, &mut target_spellings);
    let expansion = ty_of_ref(&expanded_target, names, &symbolic, diags);
    if expansion == Ty::Error {
        return None;
    }
    let spelling = spelling_of_ref(
        &expanded_target,
        names,
        &symbolic,
        known_spellings,
        &target_spellings,
    );
    Some((expansion, spelling))
}

pub(in crate::resolve) fn compact_classifier_identity(
    headers: &crate::fir::StreamedHeaderModule,
    stub: &crate::fir::DeclarationStub,
) -> Option<(String, TypeName)> {
    let source_name = headers.lookup_names.get(stub.lookup_name?)?.to_owned();
    let package = headers.sources.get(stub.source)?.package;
    // A local classifier's source path is lookup input, not a target class name. Give it an opaque
    // module-stable semantic identity; a backend consumes its declaration-keyed lexical provenance
    // and chooses the physical spelling later.
    let runtime_name = if stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS) {
        // Only a member of a local classifier's own body is nested in it. A classifier declared
        // in executable code of a member or class header (`Owner.f { object {} }`,
        // `class A(x: Any = run { class B })`) has that classifier as its lexical naming owner
        // too, but it is not a nested member and must keep its own opaque identity.
        let nested = headers
            .local_class_name_provenance
            .get(&stub.id)
            .and_then(|provenance| provenance.lexical_owner)
            .filter(|owner| {
                headers
                    .declarations
                    .anchor(stub.id)
                    .is_some_and(|anchor| anchor.owner == Some(*owner))
            })
            .and_then(|owner| headers.stub(owner))
            .filter(|owner| {
                owner.kind == crate::fir::DeclarationKind::Classifier
                    && owner.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
            })
            .and_then(|owner| compact_classifier_identity(headers, owner))
            .and_then(|(_, owner)| {
                headers
                    .local_class_name_provenance
                    .get(&stub.id)?
                    .segments
                    .last()
                    .map(|segment| crate::types::type_name_nested_child(owner, segment))
            });
        if let Some(nested) = nested {
            return Some((source_name, nested));
        }
        format!("__krusty_local_{}", stub.id.raw())
    } else {
        source_name.replace('.', "$")
    };
    Some((
        source_name,
        crate::types::type_name_child(package, &runtime_name),
    ))
}

/// Classifier roots visible from a compact declaration, nearest first.
///
/// This is the stable-identity counterpart of [`declaration_lexical_class_names`]. Anonymous
/// objects use the structural owner edges extracted while their source was active; ordinary
/// nested classifiers use their already-qualified internal identity. No parser `DeclId` is
/// reconstructed or retained.
pub(in crate::resolve) fn compact_declaration_lexical_class_names(
    headers: &crate::fir::StreamedHeaderModule,
    context: &PassOneLocalClassContext,
    declaration: crate::fir::DeclarationId,
    mut exists: impl FnMut(TypeName) -> bool,
) -> Vec<TypeName> {
    let mut stable_chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut current = Some(declaration);
    while let Some(candidate) = current {
        if !seen.insert(candidate) {
            break;
        }
        stable_chain.push(candidate);
        current = context.anonymous_owners.get(&candidate).copied();
    }

    let mut classes = Vec::new();
    for candidate in stable_chain {
        let Some(stub) = headers.stub(candidate) else {
            continue;
        };
        let Some((_, internal)) = compact_classifier_identity(headers, stub) else {
            continue;
        };
        if context.anonymous_declarations.contains(&candidate) {
            if !classes.contains(&internal) {
                classes.push(internal);
            }
        } else {
            for lexical in
                crate::symbol_resolver::lexical_enclosing_classifier_names(internal, &mut exists)
            {
                if !classes.contains(&lexical) {
                    classes.push(lexical);
                }
            }
        }
    }
    classes
}

pub(in crate::resolve) fn compact_source_package_spelling(
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
) -> Option<String> {
    let scope = headers.scopes.file(source)?;
    headers
        .scopes
        .path(scope.package)
        .iter()
        .map(|segment| headers.lookup_names.get(*segment))
        .collect::<Option<Vec<_>>>()
        .map(|segments| segments.join("."))
}

#[derive(Clone)]
pub(in crate::resolve) struct CompactSourceImport {
    pub(in crate::resolve) visible_name: String,
    pub(in crate::resolve) path: String,
    pub(in crate::resolve) wildcard: bool,
}

pub(in crate::resolve) fn compact_source_imports(
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
) -> Option<Vec<CompactSourceImport>> {
    let scope = headers.scopes.file(source)?;
    headers
        .scopes
        .imports(scope.imports)
        .iter()
        .map(|import| {
            let segments = headers
                .scopes
                .path(import.path)
                .iter()
                .map(|segment| headers.lookup_names.get(*segment))
                .collect::<Option<Vec<_>>>()?;
            let path = segments.join(".");
            let visible_name = import
                .alias
                .and_then(|alias| headers.lookup_names.get(alias))
                .or_else(|| segments.last().copied())
                .unwrap_or_default()
                .to_owned();
            Some(CompactSourceImport {
                visible_name,
                path,
                wildcard: import.wildcard,
            })
        })
        .collect()
}

/// Publish an annotation classifier's source-declared retention/target policy from compact
/// headers. The annotation argument arena stores only resolved-policy enum spellings and owns no
/// ordinary expression or parser identity.
pub(in crate::resolve) fn collect_compact_annotation_policies(
    headers: &crate::fir::StreamedHeaderModule,
    table: &mut SymbolTable,
) {
    for stub in headers.stubs.iter().filter(|stub| {
        stub.kind == crate::fir::DeclarationKind::Classifier
            && stub
                .flags
                .has(crate::fir::DeclarationFlags::ANNOTATION_CLASS)
    }) {
        let source = stub.source.raw();
        let retention = headers
            .annotation_policy_applications(stub.id)
            .iter()
            .find_map(|application| {
                table
                    .resolved_annotations
                    .get(&(source, application.annotation.lo, application.annotation.hi))
                    .filter(|name| name.matches("kotlin/annotation/Retention"))?;
                let argument = *headers
                    .annotation_policy_arguments(application.arguments)
                    .first()?;
                match headers.lookup_names.get(argument)? {
                    "RUNTIME" => Some(crate::types::AnnotationRetention::Runtime),
                    "BINARY" => Some(crate::types::AnnotationRetention::Binary),
                    "SOURCE" => Some(crate::types::AnnotationRetention::Source),
                    _ => None,
                }
            })
            .unwrap_or(crate::types::AnnotationRetention::Default);
        let (_, annotation) = compact_classifier_identity(headers, stub)
            .expect("a compact annotation classifier must retain its stable identity");
        table.annotation_retentions.insert(annotation, retention);

        let declared_targets = headers
            .annotation_policy_applications(stub.id)
            .iter()
            .find_map(|application| {
                table
                    .resolved_annotations
                    .get(&(source, application.annotation.lo, application.annotation.hi))
                    .filter(|name| name.matches("kotlin/annotation/Target"))?;
                let mut targets = crate::types::AnnotationTargets {
                    value_parameter: false,
                    property: false,
                    field: false,
                };
                for argument in headers
                    .annotation_policy_arguments(application.arguments)
                    .iter()
                    .filter_map(|argument| headers.lookup_names.get(*argument))
                {
                    match argument {
                        "VALUE_PARAMETER" => targets.value_parameter = true,
                        "PROPERTY" => targets.property = true,
                        "FIELD" => targets.field = true,
                        _ => {}
                    }
                }
                Some(targets)
            });
        if let Some(targets) = declared_targets {
            table.annotation_targets.insert(annotation, targets);
        }
    }
}

pub(in crate::resolve) fn normalize_referenced_library_annotations(
    table: &mut SymbolTable,
    libraries: &dyn SemanticPlatform,
) {
    let referenced = table
        .resolved_annotations
        .values()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for annotation in referenced {
        if table.annotation_retentions.contains_key(&annotation) {
            continue;
        }
        let Some(library) = libraries.classifier(annotation) else {
            continue;
        };
        if !library.is_annotation() {
            continue;
        }
        if let Some(targets) = library.annotation_targets {
            table
                .annotation_targets
                .entry(annotation)
                .or_insert(targets);
        }
        let retention = match library.retention.as_deref() {
            Some("RUNTIME") => crate::types::AnnotationRetention::Runtime,
            Some("CLASS") => crate::types::AnnotationRetention::Binary,
            Some("SOURCE") => crate::types::AnnotationRetention::Source,
            _ => continue,
        };
        table.annotation_retentions.insert(annotation, retention);
    }
}
