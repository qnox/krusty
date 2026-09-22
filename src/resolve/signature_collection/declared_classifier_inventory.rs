//! Classifier facts the declaration walk needs before it starts.
//!
//! A supertype spelling, a classifier's visibility, and (on the non-streamed path) the lexical
//! views of local and anonymous classes are all decided from declarations alone. They are
//! inventoried here, over the whole source set, because a parenless supertype in one file may name
//! a classifier declared in another and the walk must not reopen that question per declaration.

use super::*;

/// Enclosing type parameters of one file's local classes, by declaration.
type LocalClassEnclosingTParams = HashMap<DeclId, Vec<EnclosingTypeParameterDeclaration>>;

/// Sibling classifier names visible to one file's local classes, by declaration.
type LocalClassSiblingNames = HashMap<DeclId, Vec<(String, TypeName)>>;

/// The declaration-derived classifier facts, collected once for the whole source set.
pub(in crate::resolve) struct DeclaredClassifierInventory {
    /// Lexical scope of each file's anonymous classes. `None` on the compact path, which keeps the
    /// stable Pass-1 context instead of reconstructing a `DeclId` from a source range.
    pub(in crate::resolve) legacy_anonymous_lexical_scopes: Option<Vec<AnonymousLexicalClassScope>>,
    /// Enclosing type parameters of each file's local classes; `None` on the compact path.
    pub(in crate::resolve) legacy_local_class_tparams: Option<Vec<LocalClassEnclosingTParams>>,
    /// Sibling names of each file's local classes; `None` on the compact path.
    pub(in crate::resolve) legacy_local_class_siblings: Option<Vec<LocalClassSiblingNames>>,
    /// Declared visibility of every source classifier, by qualified identity.
    pub(in crate::resolve) source_classifier_visibility: HashMap<TypeName, Visibility>,
    /// Direct supertypes each source classifier was written with, already bound to qualified
    /// identities.
    pub(in crate::resolve) source_direct_supertypes: HashMap<TypeName, Vec<TypeName>>,
}

pub(in crate::resolve) fn declared_classifier_inventory(
    files: &[File],
    compact_headers: Option<&crate::fir::StreamedHeaderModule>,
    compact_local_contexts: Option<&[PassOneLocalClassContext]>,
    file_class_names: &[ClassNames],
    user_defined: &std::collections::HashSet<TypeName>,
) -> DeclaredClassifierInventory {
    // Inspection-only lexical views remain parser-keyed. Production keeps the stable Pass-1
    // context directly and never reconstructs a `DeclId` from a source range.
    let legacy_anonymous_lexical_scopes = compact_headers.is_none().then(|| {
        files
            .iter()
            .map(anonymous_lexical_class_scope)
            .collect::<Vec<_>>()
    });
    let legacy_local_class_tparams = compact_headers.is_none().then(|| {
        files
            .iter()
            .map(local_class_enclosing_tparams)
            .collect::<Vec<_>>()
    });
    let legacy_local_class_siblings = compact_headers.is_none().then(|| {
        files
            .iter()
            .map(local_class_sibling_names)
            .collect::<Vec<_>>()
    });
    let source_classifier_visibility = compact_headers.map_or_else(
        || {
            files
                .iter()
                .flat_map(|file| {
                    file.decls.iter().filter_map(|&declaration| {
                        let Decl::Class(class) = file.decl(declaration) else {
                            return None;
                        };
                        Some((
                            type_name(&class_internal(file, &class.name)),
                            class.visibility,
                        ))
                    })
                })
                .collect::<HashMap<_, _>>()
        },
        |headers| {
            headers
                .stubs
                .iter()
                .filter(|stub| stub.kind == crate::fir::DeclarationKind::Classifier)
                .map(|stub| {
                    let (_, identity) = compact_classifier_identity(headers, stub).expect(
                        "a compact classifier must retain its lookup name and source package",
                    );
                    (identity, stub.visibility)
                })
                .collect()
        },
    );
    let mut source_direct_supertypes: HashMap<TypeName, Vec<TypeName>> = HashMap::new();
    if let Some(headers) = compact_headers {
        let empty_context = PassOneLocalClassContext::default();
        for stub in headers
            .stubs
            .iter()
            .filter(|stub| stub.kind == crate::fir::DeclarationKind::Classifier)
        {
            let source = stub.source.raw() as usize;
            let context = compact_local_contexts
                .and_then(|contexts| contexts.get(source))
                .unwrap_or(&empty_context);
            // Parser-hoisted local classifiers keep their source header, but their generated
            // module name is deliberately not entered under the source spelling in the file-wide
            // import table. Extend this compact header-only view with classifiers from the
            // declaration's lexical body before recording hierarchy edges.
            let mut names = file_class_names[source].clone();
            for (simple, internal) in context
                .sibling_classifiers
                .get(&stub.id)
                .into_iter()
                .flatten()
            {
                names.insert_name(simple.clone(), *internal);
            }
            let classifier_header = streamed_classifier_header_by_declaration(headers, stub.id)
                .expect("a production classifier must have a compact header");
            let (source_name, internal) = compact_classifier_identity(headers, stub)
                .expect("a compact classifier must retain its stable identity");
            let lexical_classifiers =
                compact_declaration_lexical_class_names(headers, context, stub.id, |candidate| {
                    user_defined.contains(&candidate)
                });
            let mut supertypes = classifier_header
                .supertypes
                .iter()
                .filter_map(|supertype| {
                    declared_supertype_name_from_source_name(
                        &source_name,
                        &supertype.name,
                        &names,
                        &lexical_classifiers,
                    )
                })
                .collect::<Vec<_>>();
            supertypes.extend(classifier_header.base.as_ref().and_then(|base| {
                declared_supertype_name_from_source_name(
                    &source_name,
                    &base.name,
                    &names,
                    &lexical_classifiers,
                )
            }));
            if stub
                .flags
                .has(crate::fir::DeclarationFlags::ANNOTATION_CLASS)
            {
                supertypes.push(type_name("kotlin/Annotation"));
            }
            source_direct_supertypes.insert(internal, supertypes);
        }
    } else {
        for (file_index, file) in files.iter().enumerate() {
            for &declaration in &file.decls {
                let Decl::Class(class) = file.decl(declaration) else {
                    continue;
                };
                let mut names = file_class_names[file_index].clone();
                for (simple, internal) in legacy_local_class_siblings
                    .as_ref()
                    .expect("legacy local classifier scopes")[file_index]
                    .get(&declaration)
                    .into_iter()
                    .flatten()
                {
                    names.insert_name(simple.clone(), *internal);
                }
                let classifier_header = legacy_classifier_header(class);
                let internal = type_name(&class_internal(file, &class.name));
                let lexical_classifiers = declaration_lexical_class_names(
                    file,
                    declaration,
                    &legacy_anonymous_lexical_scopes
                        .as_ref()
                        .expect("legacy anonymous classifier scopes")[file_index],
                    |candidate| user_defined.contains(&candidate),
                );
                let mut supertypes = classifier_header
                    .supertypes
                    .iter()
                    .filter_map(|supertype| {
                        declared_supertype_name(
                            class,
                            &supertype.name,
                            &names,
                            &lexical_classifiers,
                        )
                    })
                    .collect::<Vec<_>>();
                supertypes.extend(classifier_header.base.as_ref().and_then(|base| {
                    declared_supertype_name(class, &base.name, &names, &lexical_classifiers)
                }));
                supertypes.extend(implicit_source_supertypes(class));
                source_direct_supertypes.insert(internal, supertypes);
            }
        }
    }
    DeclaredClassifierInventory {
        legacy_anonymous_lexical_scopes,
        legacy_local_class_tparams,
        legacy_local_class_siblings,
        source_classifier_visibility,
        source_direct_supertypes,
    }
}
