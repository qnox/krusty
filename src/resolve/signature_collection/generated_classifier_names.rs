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

/// The header of a source class that the plugins decide from, bound once, and the nested
/// classifiers they generated from it. Every plugin hook of the class reads this one header.
pub(in crate::resolve) struct ClassifierPluginFacts {
    classifier: TypeName,
    companion: Option<TypeName>,
    kind: crate::libraries::TypeKind,
    is_sealed: bool,
    is_abstract: bool,
    /// The class's annotations, bound through the file's classifier names before any type is
    /// resolved, the way kotlinc binds the annotations a plugin's predicates read.
    annotations: Vec<TypeName>,
    /// A class-valued annotation argument, resolved next to the annotation's own name and through
    /// the same classifier rules. Another file of this module reads it from the stable index; it
    /// never sees this declaration's syntax, so recovering the identity from a spelling later is
    /// not available to it.
    pub(in crate::resolve) annotation_class_arguments: Vec<(u32, TypeName)>,
    pub(in crate::resolve) generated: Vec<crate::types::GeneratedClassifierFact>,
}

impl ClassifierPluginFacts {
    pub(in crate::resolve) fn header(&self) -> crate::plugins::FrontendClassifierHeader<'_> {
        crate::plugins::FrontendClassifierHeader {
            classifier: self.classifier,
            companion: self.companion,
            kind: self.kind,
            is_sealed: self.is_sealed,
            is_abstract: self.is_abstract,
            annotations: &self.annotations,
            annotation_class_arguments: &self.annotation_class_arguments,
        }
    }
}

/// The plugins' generated nested classifiers of every source class, decided once from the class's
/// header. The type universe declares the reachable ones; signature collection records each class's
/// own facts from here and never asks the plugins again.
#[derive(Default)]
pub(in crate::resolve) struct GeneratedClassifiers {
    by_declaration: HashMap<(usize, DeclId), ClassifierPluginFacts>,
    reachable: Vec<GeneratedSourceClassifier>,
}

impl GeneratedClassifiers {
    /// The plugin facts of class declaration `declaration` of source `source`. Signature
    /// collection visits exactly the classes the universe build did.
    pub(in crate::resolve) fn take(
        &mut self,
        source: usize,
        declaration: DeclId,
    ) -> ClassifierPluginFacts {
        self.by_declaration
            .remove(&(source, declaration))
            .expect("the universe build records the plugin facts of every collected class")
    }
}

/// The source type universe with the plugins' generated nested classifiers declared, so a written
/// `Owner.Companion` binds to the companion a plugin generates, together with those classifiers.
pub(in crate::resolve) fn plugin_source_type_universe(
    files: &[File],
    libraries: &dyn SemanticPlatform,
    diags: &mut DiagSink,
    compact_headers: Option<&crate::fir::StreamedHeaderModule>,
    compact_local_contexts: Option<&[PassOneLocalClassContext]>,
    plugins: &crate::plugins::PluginHost,
) -> (SourceTypeUniverse, GeneratedClassifiers) {
    let universe = source_type_universe(files, libraries, diags, compact_headers, &[]);
    let generated = plugin_generated_classifiers(
        files,
        compact_headers,
        compact_local_contexts,
        &universe,
        plugins,
    );
    if !universe_misses_generated_classifier(&universe, &generated.reachable) {
        return (universe, generated);
    }
    // The first build already reported its declaration conflicts; the generated classifiers add
    // none, so the second build's diagnostics repeat them.
    let universe = source_type_universe(
        files,
        libraries,
        &mut DiagSink::new(),
        compact_headers,
        &generated.reachable,
    );
    (universe, generated)
}

pub(in crate::resolve) fn plugin_generated_classifiers(
    files: &[File],
    compact_headers: Option<&crate::fir::StreamedHeaderModule>,
    compact_local_contexts: Option<&[PassOneLocalClassContext]>,
    universe: &SourceTypeUniverse,
    plugins: &crate::plugins::PluginHost,
) -> GeneratedClassifiers {
    let mut generated = GeneratedClassifiers::default();
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
            let reachable = !stub.is_some_and(|stub| {
                stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
                    || stub
                        .flags
                        .has(crate::fir::DeclarationFlags::ANONYMOUS_OBJECT)
            });
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
            let mut facts = ClassifierPluginFacts {
                classifier,
                companion,
                kind: frontend_class_kind(flags, is_enum),
                is_sealed: flags.has(ClassFlags::SEALED),
                is_abstract: flags.has(ClassFlags::ABSTRACT),
                annotations,
                annotation_class_arguments,
                generated: Vec::new(),
            };
            let mut generated_here = Vec::new();
            plugins.publish_frontend_generated_classifiers(&facts.header(), &mut generated_here);
            facts.generated = generated_here;
            if reachable {
                generated
                    .reachable
                    .extend(
                        facts
                            .generated
                            .iter()
                            .map(|fact| GeneratedSourceClassifier {
                                source_name: format!("{}.{}", class.name, fact.source_name),
                                simple_name: fact.source_name.to_string(),
                                identity: fact.classifier,
                            }),
                    );
            }
            generated
                .by_declaration
                .insert((source, declaration), facts);
        }
    }
    generated
}

/// Whether a written type of some file stopped at a generated name, so the universe has to be built
/// again with the generated classifiers declared.
fn universe_misses_generated_classifier(
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
