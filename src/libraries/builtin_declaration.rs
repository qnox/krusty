//! Normalized semantic declarations read from Kotlin builtin metadata.
//!
//! Providers may obtain these declarations from different containers, but realization is allowed
//! to inspect only this common semantic record: the qualified owner identity, the complete declared
//! signature, and declaration modifiers. JVM descriptors, KLIB table ordinals, and provider-local
//! handles are deliberately absent.

use crate::libraries::{FnKind, PropKind};
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

/// One exact package-level Kotlin function after a provider has normalized its declaration.
///
/// The package is the source namespace, never a JVM file facade. Receivers and parameters are
/// semantic Kotlin types, and the receiver is not repeated in `params`.
pub(crate) struct BuiltinFunctionDeclaration<'a> {
    pub package: TypeName,
    pub name: &'a str,
    pub kind: FnKind,
    pub receiver: Option<Ty>,
    pub params: &'a [Ty],
    pub ret: Ty,
    pub context_count: usize,
    pub type_parameter_count: usize,
    pub vararg: Option<usize>,
    pub is_suspend: bool,
    pub is_operator: bool,
    pub is_infix: bool,
}

/// One exact package-level Kotlin property after provider normalization.
pub(crate) struct BuiltinPropertyDeclaration<'a> {
    pub package: TypeName,
    pub name: &'a str,
    pub kind: PropKind,
    pub receiver: Option<Ty>,
    pub ty: Ty,
    pub context_count: usize,
    pub type_parameter_count: usize,
    pub mutable: bool,
}
