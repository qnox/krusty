//! Normalized semantic declarations read from Kotlin builtin metadata.
//!
//! Providers may obtain these declarations from different containers, but realization is allowed
//! to inspect only this common semantic record: the qualified owner identity, the complete declared
//! signature, and declaration modifiers. JVM descriptors, KLIB table ordinals, and provider-local
//! handles are deliberately absent.

use crate::types::{Ty, TypeName};

/// One exact builtin member declaration at a provider boundary.
///
/// The textual callable name is one component of the declaration, never its identity by itself.
/// Consumers must match the complete record; a later KLIB provider can construct the same record
/// only after its linkdata declaration has been joined to IR by the KLIB's stable `IdSignature`.
pub(crate) struct BuiltinMemberDeclaration<'a> {
    pub owner: TypeName,
    pub name: &'a str,
    pub params: &'a [Ty],
    pub ret: Ty,
    pub is_property: bool,
    pub is_operator: bool,
    pub is_infix: bool,
}
