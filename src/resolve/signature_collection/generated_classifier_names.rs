//! Nested classifiers a frontend plugin generates, published to the source type universe.
//!
//! kotlinc's FIR asks each declaration-generation extension for the nested classifier names of a
//! class before it resolves any type, so `Owner.Generated` binds like a nested classifier written
//! in source. The serialization plugin generates `Companion` for a `@Serializable` class without
//! one: `val Owner.Companion.x` is an extension on it. A plugin decides from the class's header and
//! its resolved annotations, so the universe is built once to bind those annotations; it is built
//! again with the generated names only when a written type stopped at one of them.

use super::*;

/// A generated nested classifier, under the source spelling a type reference writes for it.
pub(in crate::resolve) struct GeneratedSourceClassifier {
    /// `Owner.Companion`: the owner's source spelling and the generated simple name.
    pub(in crate::resolve) source_name: String,
    pub(in crate::resolve) simple_name: String,
    pub(in crate::resolve) identity: TypeName,
}

/// Every nested classifier the enabled plugins generate for a source class of `files`. A local or
/// anonymous class is not reachable through a qualified type, so it contributes none here.
pub(in crate::resolve) fn plugin_generated_classifiers(
    files: &[File],
    compact_headers: Option<&crate::fir::StreamedHeaderModule>,
    compact_local_contexts: Option<&[PassOneLocalClassContext]>,
    universe: &SourceTypeUniverse,
    plugins: &crate::plugins::PluginHost,
) -> Vec<GeneratedSourceClassifier> {
    let mut generated = Vec::new();
    for (source, file) in files.iter().enumerate() {
        let names = &universe.file_class_names[source];
        for &declaration in &file.decls {
            let Decl::Class(class) = file.decl(declaration) else {
                continue;
            };
            let stub = compact_headers.and_then(|headers| {
                compact_parser_declaration_stub(
                    headers,
                    compact_local_contexts,
                    source,
                    declaration,
                )
            });
            if compact_headers.is_some() && stub.is_none() {
                continue;
            }
            if stub.is_some_and(|stub| {
                stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    || stub
                        .flags
                        .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
            }) {
                continue;
            }
            let flags = compact_headers.zip(stub).map_or_else(
                || source_class_flags(class),
                |(headers, stub)| streamed_source_class_flags(headers, stub),
            );
            let is_enum = stub.map_or(class.is_enum(), |stub| {
                stub.flags.has(crate::fir::DeclarationFlags::ENUM)
            });
            let classifier = projected_classifier_identity(
                file,
                compact_headers,
                stub,
                &class.name,
                &universe.user_defined,
                names,
            );
            let companion = class.companion.and_then(|companion| {
                projected_parser_classifier_identity(
                    file,
                    compact_headers,
                    compact_local_contexts,
                    source,
                    companion,
                )
            });
            let annotations = class
                .annotations
                .iter()
                .filter_map(|annotation| names.annotation_binding(&annotation.name))
                .map(|binding| binding.classifier)
                .collect::<Vec<_>>();
            let annotation_class_arguments = compact_headers
                .zip(stub)
                .map(|(headers, stub)| {
                    streamed_declaration_annotation_class_literals(headers, stub.id)
                })
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(ordinal, path)| {
                    Some((ordinal, names.classifier_binding(&path).ok()?))
                })
                .collect::<Vec<_>>();
            let mut facts_out = Vec::new();
            plugins.publish_frontend_generated_classifiers(
                &crate::plugins::FrontendClassifierHeader {
                    classifier,
                    companion,
                    kind: frontend_class_kind(flags, is_enum),
                    is_sealed: flags.has(ClassFlags::SEALED),
                    is_abstract: flags.has(ClassFlags::ABSTRACT),
                    annotations: &annotations,
                    annotation_class_arguments: &annotation_class_arguments,
                },
                &mut facts_out,
            );
            generated.extend(facts_out.into_iter().map(|fact| GeneratedSourceClassifier {
                source_name: format!("{}.{}", class.name, fact.source_name),
                simple_name: fact.source_name.to_string(),
                identity: fact.classifier,
            }));
        }
    }
    generated
}

/// Whether a written type of some file stopped at a generated name, so the universe has to be built
/// again with the generated classifiers declared.
pub(in crate::resolve) fn universe_misses_generated_classifier(
    universe: &SourceTypeUniverse,
    generated: &[GeneratedSourceClassifier],
) -> bool {
    universe.file_class_names.iter().any(|names| {
        names.unresolved_segments.values().any(|segment| {
            generated
                .iter()
                .any(|classifier| classifier.simple_name == *segment)
        })
    })
}
