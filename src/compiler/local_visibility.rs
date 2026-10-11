//! Body-group scheduling after a local classifier publishes inherited member visibility.

use crate::ast::{Decl, File};
use crate::fir::{
    ActiveSourceDeclarations, DeclarationFlags, DeclarationKind, ResolvedModuleIndex,
};
use crate::resolve::TypeInfo;
use crate::types::Visibility;

/// Whether this checked group published a modifier-less local member as non-public.
///
/// The first traversal discovers and publishes local signatures. Only such a visibility change
/// requires a second access check of the enclosing body; ordinary local classes remain final after
/// the first traversal and must not be treated as a second declaration.
pub(super) fn group_published_non_public_member(
    file: &File,
    active: &ActiveSourceDeclarations,
    info: &TypeInfo,
    index: &ResolvedModuleIndex,
) -> bool {
    info.checked_local_class_declarations
        .iter()
        .filter_map(|declaration| match file.decl(*declaration) {
            Decl::Class(_) => active.canonical_classifier_declaration(*declaration, index),
            Decl::Fun(_) | Decl::Property(_) => None,
        })
        .flat_map(|classifier| index.owned_declarations(classifier))
        .filter_map(|declaration| index.declaration_header(*declaration))
        .any(|header| {
            matches!(
                header.kind,
                DeclarationKind::Function | DeclarationKind::Property
            ) && !header.flags.has(DeclarationFlags::HAS_VISIBILITY_MODIFIER)
                && header.visibility != Visibility::Public
        })
}
