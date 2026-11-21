//! The nested classifiers a frontend plugin generates, named while the type universe is built.
//!
//! A source spelling such as `Type.Companion` binds to a classifier through the universe's
//! name-resolution pass, long before the plugin sees the class's members. The plugin therefore
//! names what it will generate from the class's resolved annotations, kind and companion alone,
//! and the pass binds the spelling to that identity.

use crate::resolve::*;

/// The identities every source class's plugins generate, given each file's resolved spellings.
///
/// `resolved_by_file[i]` maps a spelling written in file `i` to the classifier its scope and
/// imports selected; an annotation spelling absent from it did not resolve and contributes nothing.
pub(super) fn generated_classifier_identities(
    files: &[File],
    compact_headers: Option<&crate::fir::StreamedHeaderModule>,
    plugins: &crate::plugins::PluginHost,
    resolved_by_file: &[&HashMap<String, TypeName>],
) -> crate::fx_hash::HashSet<TypeName> {
    let mut identities = crate::fx_hash::HashSet::default();
    let mut generate = |classifier: TypeName,
                        kind: crate::libraries::TypeKind,
                        has_companion: bool,
                        annotations: Vec<TypeName>| {
        identities.extend(plugins.generated_classifier_names(
            &crate::plugins::FrontendClassNameContext {
                classifier,
                kind,
                has_companion,
                annotations: &annotations,
            },
        ));
    };
    if let Some(headers) = compact_headers {
        let owners_with_companion: crate::fx_hash::HashSet<crate::fir::DeclarationId> = headers
            .stubs
            .iter()
            .filter(|stub| {
                stub.kind == crate::fir::DeclarationKind::Classifier
                    && stub.flags.has(crate::fir::DeclarationFlags::COMPANION)
            })
            .filter_map(|stub| headers.declarations.anchor(stub.id)?.owner)
            .collect();
        for stub in headers.stubs.iter().filter(|stub| {
            stub.kind == crate::fir::DeclarationKind::Classifier
                && !stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
        }) {
            let Some((_, classifier)) = compact_classifier_identity(headers, stub) else {
                continue;
            };
            let Some(declaration) = headers.syntax.declaration(stub.id) else {
                continue;
            };
            let resolved = resolved_by_file[stub.source.raw() as usize];
            let annotations = headers
                .syntax
                .type_operands(declaration.annotations)
                .iter()
                .filter_map(|annotation| {
                    let reference = headers
                        .syntax
                        .transient_type_ref(*annotation, &headers.lookup_names)?;
                    resolved.get(&reference.name).copied()
                })
                .collect();
            generate(
                classifier,
                compact_stub_kind(stub.flags),
                owners_with_companion.contains(&stub.id),
                annotations,
            );
        }
    } else {
        for (file_index, file) in files.iter().enumerate() {
            let resolved = resolved_by_file[file_index];
            for &declaration in &file.decls {
                let Decl::Class(class) = file.decl(declaration) else {
                    continue;
                };
                let annotations = class
                    .annotations
                    .iter()
                    .filter_map(|annotation| resolved.get(&annotation.name).copied())
                    .collect();
                let kind = match class.kind {
                    crate::ast::ClassKind::Class if class.is_singleton() => {
                        crate::libraries::TypeKind::Object
                    }
                    crate::ast::ClassKind::Class => crate::libraries::TypeKind::Class,
                    crate::ast::ClassKind::Interface => crate::libraries::TypeKind::Interface,
                    crate::ast::ClassKind::Enum => crate::libraries::TypeKind::Enum,
                    crate::ast::ClassKind::Annotation => crate::libraries::TypeKind::Annotation,
                };
                generate(
                    type_name(&class_internal(file, &class.name)),
                    kind,
                    class.companion.is_some(),
                    annotations,
                );
            }
        }
    }
    identities
}

/// The classifier kind a plugin sees, in the precedence `frontend_class_kind` gives the same flags.
fn compact_stub_kind(flags: crate::fir::DeclarationFlags) -> crate::libraries::TypeKind {
    use crate::fir::DeclarationFlags as Flag;
    if flags.has(Flag::ANNOTATION_CLASS) {
        crate::libraries::TypeKind::Annotation
    } else if flags.has(Flag::SINGLETON) {
        crate::libraries::TypeKind::Object
    } else if flags.has(Flag::ENUM) {
        crate::libraries::TypeKind::Enum
    } else if flags.has(Flag::INTERFACE) {
        crate::libraries::TypeKind::Interface
    } else {
        crate::libraries::TypeKind::Class
    }
}
