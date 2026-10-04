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

/// The annotations `@Metadata` records on each annotated type occurrence of one compact source:
/// of the annotations written on the occurrence, in source order, those Pass 1 bound to a
/// classifier whose retention is not `SOURCE`. See [`crate::spelling::Spelled::annotations`].
pub(in crate::resolve) fn compact_source_type_annotations(
    table: &SymbolTable,
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
) -> crate::spelling::RecordedTypeAnnotations {
    let mut recorded = crate::spelling::RecordedTypeAnnotations::default();
    for (occurrence, annotations) in headers.type_use_annotations(source) {
        let identities = annotations
            .iter()
            .filter(|annotation| !annotation.has_arguments)
            .filter_map(|annotation| {
                let reference = AnnotationRef {
                    name: String::new(),
                    span: annotation.annotation,
                };
                table.resolved_annotation(source.raw(), &reference)
            })
            .filter(|&identity| {
                let retention = table.annotation_retention(identity).or_else(|| {
                    let classifier = table.libraries.classifier(identity)?;
                    crate::resolve::annotation_applications::annotation_retention(None, &classifier)
                });
                retention
                    .is_some_and(|retention| retention != crate::types::AnnotationRetention::Source)
            })
            .collect();
        recorded.record(occurrence, identities);
    }
    recorded
}
