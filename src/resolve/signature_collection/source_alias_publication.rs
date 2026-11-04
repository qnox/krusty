//! Publication of source `typealias` declarations: each alias's expansion, target classifier, and
//! the spelling template of its right-hand side as written.
//!
//! The template is what a use site expands (`Cargo` abbreviates where `typealias Box = PBox<Cargo>`
//! is used) and what the alias's own `@Metadata` records. Like any declared type occurrence it
//! carries the type-use annotations its right-hand side wrote: kotlinc records them on the alias's
//! `underlying_type` and `expanded_type`, and on the expanded type of every use.

use super::*;

/// One source alias declaration to publish under its qualified identity.
pub(in crate::resolve) struct SourceAlias<'a> {
    pub(in crate::resolve) identity: TypeName,
    pub(in crate::resolve) declaration_name: crate::types::ClassifierDeclarationName,
    /// The classifier whose body declares the alias, and the alias's name there; `None` for a
    /// top-level alias.
    pub(in crate::resolve) nested_in: Option<(TypeName, &'a str)>,
    pub(in crate::resolve) formals: &'a [String],
    pub(in crate::resolve) target: &'a TypeRef,
}

/// What the alias's right-hand side is resolved against: the aliases and classifiers visible at
/// its declaration and the type-use annotations its file recorded.
pub(in crate::resolve) struct SourceAliasScope<'a> {
    pub(in crate::resolve) visible_aliases: &'a [(String, Vec<String>, TypeRef)],
    pub(in crate::resolve) names: &'a ClassNames,
    pub(in crate::resolve) annotations: &'a crate::spelling::RecordedTypeAnnotations,
}

/// Resolve `alias` and publish its expansion, its target classifier, and its spelling template.
/// An alias whose right-hand side does not resolve publishes nothing; its diagnostic is reported.
pub(in crate::resolve) fn publish_source_alias(
    table: &mut SymbolTable,
    alias: SourceAlias<'_>,
    scope: SourceAliasScope<'_>,
    diags: &mut DiagSink,
) {
    let symbolic = TParams::symbolic_from_decl_with(alias.formals, &[], &|candidate| {
        scope.names.get_class(candidate)
    });
    let mut target_spellings = HashMap::default();
    let expanded_target = crate::parser::expanded_type_alias_target(
        scope.visible_aliases,
        alias.target,
        &mut target_spellings,
    );
    let expansion = ty_of_ref(&expanded_target, scope.names, &symbolic, diags);
    if expansion == Ty::Error {
        return;
    }
    let spellings = crate::spelling::SourceSpellings {
        aliases: &target_spellings,
        annotations: scope.annotations,
    };
    let spelling = spelling_of_ref(
        &expanded_target,
        scope.names,
        &symbolic,
        &table.alias_expansion_spellings,
        spellings,
    );
    if let Some(target) = crate::libraries::type_alias_target_classifier(expansion) {
        table.source_alias_fqns.insert(alias.identity, target);
    }
    if let Some((owner, name)) = alias.nested_in {
        table
            .nested_type_aliases
            .insert((owner, name.to_owned()), alias.identity);
    }
    let formals = alias.formals.to_vec();
    table.source_alias_expansions.insert(
        alias.identity,
        crate::libraries::TypeAliasTemplate {
            declaration_name: Some(alias.declaration_name),
            formals: formals.clone(),
            expansion,
        },
    );
    // Recorded unconditionally: even a right-hand side that spells no alias carries the formals and
    // expansion a use site needs to place ITS spellings into the parameter positions (`typealias
    // Boxed<T> = PBox<T, T>` spells nothing, yet `Boxed<Cargo>` abbreviates both expanded arguments).
    table
        .alias_expansion_spellings
        .insert(alias.identity, (spelling, formals, expansion));
}

/// Every type-use annotation application of one compact source, as Pass 1 bound it, and the name
/// of each named function-type parameter. An application's arguments and its retention are the
/// checker's to decide; see [`crate::spelling::TypeUseAnnotation`].
pub(in crate::resolve) fn compact_source_type_annotations(
    table: &SymbolTable,
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
) -> crate::spelling::RecordedTypeAnnotations {
    let mut recorded = crate::spelling::RecordedTypeAnnotations::default();
    for (occurrence, name) in headers.function_type_parameter_names(source) {
        recorded.record_parameter_name(occurrence, name);
    }
    for (occurrence, annotations) in headers.type_use_annotations(source) {
        let bound = annotations
            .iter()
            .filter(|&&span| {
                let reference = AnnotationRef {
                    name: String::new(),
                    at: span.lo,
                    span,
                    property_site: None,
                };
                table
                    .resolved_annotation(source.raw(), &reference)
                    .is_some()
            })
            .map(|&span| {
                crate::spelling::TypeUseAnnotation::Bound(crate::spelling::AnnotationOccurrence {
                    source: source.raw(),
                    span,
                })
            })
            .collect();
        recorded.record(occurrence, bound);
    }
    recorded
}

/// Every source file's declared aliases and the package and imports each file resolves them in.
pub(in crate::resolve) struct SourceAliasDeclarations<'a> {
    pub(in crate::resolve) aliases: &'a [Vec<(String, Vec<String>, TypeRef)>],
    pub(in crate::resolve) packages: &'a [String],
    pub(in crate::resolve) package_ids: &'a [TypeName],
    pub(in crate::resolve) imports: &'a [Vec<CompactSourceImport>],
}

/// The aliases an alias right-hand side in source `file_index` is resolved against, in ascending
/// precedence: qualified spellings, star imports, same-package declarations, explicit imports.
pub(in crate::resolve) fn visible_source_aliases(
    table: &SymbolTable,
    declarations: SourceAliasDeclarations<'_>,
    file_index: usize,
) -> Vec<(String, Vec<String>, TypeRef)> {
    let package_source = &declarations.packages[file_index];
    let mut visible_aliases = Vec::new();
    // A source sees only the aliases of its own fragment and of the fragments it refines: a
    // common alias over an `expect` class expands to that class, not to its actual typealias.
    let fragment_sees = |declaration_file_index: usize, name: &str| {
        table.source_fragments.sees_alias(
            Some(file_index as u32),
            crate::types::type_name_child(declarations.package_ids[declaration_file_index], name),
        )
    };
    // Qualified spellings are absolute and independent of import precedence.
    for (declaration_file_index, declaration_package) in declarations.packages.iter().enumerate() {
        for (name, formals, target) in &declarations.aliases[declaration_file_index] {
            if !fragment_sees(declaration_file_index, name) {
                continue;
            }
            let qualified = if declaration_package.is_empty() {
                name.clone()
            } else {
                format!("{declaration_package}.{name}")
            };
            visible_aliases.push((qualified, formals.clone(), target.clone()));
        }
    }
    // Star imports contribute below same-package declarations.
    for imported_package in declarations.imports[file_index]
        .iter()
        .filter(|import| import.wildcard)
        .map(|import| import.path.as_str())
    {
        for (declaration_file_index, _) in declarations
            .packages
            .iter()
            .enumerate()
            .filter(|candidate| candidate.1.as_str() == imported_package)
        {
            visible_aliases.extend(
                declarations.aliases[declaration_file_index]
                    .iter()
                    .filter(|(name, _, _)| fragment_sees(declaration_file_index, name))
                    .cloned(),
            );
        }
    }
    // Same-package declarations outrank star imports and include aliases from sibling files.
    for (declaration_file_index, _) in declarations
        .packages
        .iter()
        .enumerate()
        .filter(|candidate| candidate.1 == package_source)
    {
        visible_aliases.extend(
            declarations.aliases[declaration_file_index]
                .iter()
                .filter(|(name, _, _)| fragment_sees(declaration_file_index, name))
                .cloned(),
        );
    }
    // Explicit imports are final and may rename the lookup spelling.
    for import in declarations.imports[file_index]
        .iter()
        .filter(|import| !import.wildcard)
    {
        let spelling = &import.visible_name;
        let imported = super::super::source_package::identity(Some(import.path.as_str()));
        for (declaration_file_index, &declaration_package) in
            declarations.package_ids.iter().enumerate()
        {
            for (name, formals, target) in &declarations.aliases[declaration_file_index] {
                if crate::types::type_name_child(declaration_package, name) == imported
                    && fragment_sees(declaration_file_index, name)
                {
                    visible_aliases.push((spelling.clone(), formals.clone(), target.clone()));
                }
            }
        }
    }
    visible_aliases
}
