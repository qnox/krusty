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
    let mut target_spellings = HashMap::new();
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
    let formals = alias.formals.to_vec();
    table
        .source_alias_expansions
        .insert(alias.identity, (formals.clone(), expansion));
    // Recorded unconditionally: even a right-hand side that spells no alias carries the formals and
    // expansion a use site needs to place ITS spellings into the parameter positions (`typealias
    // Boxed<T> = PBox<T, T>` spells nothing, yet `Boxed<Cargo>` abbreviates both expanded arguments).
    table
        .alias_expansion_spellings
        .insert(alias.identity, (spelling, formals, expansion));
}

/// Every type-use annotation application of one compact source, as Pass 1 bound it. Its
/// arguments and its retention are the checker's to decide; see
/// [`crate::spelling::TypeUseAnnotation`].
pub(in crate::resolve) fn compact_source_type_annotations(
    table: &SymbolTable,
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
) -> crate::spelling::RecordedTypeAnnotations {
    let mut recorded = crate::spelling::RecordedTypeAnnotations::default();
    for (occurrence, annotations) in headers.type_use_annotations(source) {
        let bound = annotations
            .iter()
            .filter(|&&span| {
                let reference = AnnotationRef {
                    name: String::new(),
                    span,
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
