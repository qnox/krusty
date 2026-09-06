//! Stable override decisions published while the declaration providers are live.
//!
//! An override edge is a Kotlin semantic fact. It records the exact declarations and the raw and
//! applied language types; it does not record a JVM descriptor, erased storage type, accessor
//! spelling, or bridge method. A backend may use the edge to decide whether its representation
//! requires a bridge, but it must not repeat property lookup or infer an override from a name.

use super::{CallableId, ExternalCallableId, PropertyId, ResolvedTy};
use crate::types::TypeName;

/// Stable identity of the overridden property declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResolvedPropertyOverrideTarget {
    Module(PropertyId),
    External(ExternalCallableId),
}

/// One exact property override selected in Pass 1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPropertyOverride {
    /// The source property that implements this edge.
    pub implementation: ResolvedPropertyOverrideTarget,
    pub implementation_owner: TypeName,
    /// Exact declaration being overridden, independent of its source spelling.
    pub overridden: ResolvedPropertyOverrideTarget,
    pub overridden_owner: TypeName,
    pub overridden_is_interface: bool,
    pub name: Box<str>,
    /// Declaration-side type before applying the implementing class's supertype arguments. A target
    /// backend erases this type according to its own representation rules.
    pub declared_type: ResolvedTy,
    /// The same property as viewed through the implementing class's applied supertype.
    pub applied_type: ResolvedTy,
    /// Declaration-side type of the implementation before applying the inheriting class's type
    /// arguments. This is the type a representation backend erases for the target accessor.
    pub implementation_type: ResolvedTy,
    pub overridden_mutable: bool,
    pub implementation_mutable: bool,
    pub depth: u32,
}

/// Stable identity of the overridden function declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResolvedFunctionOverrideTarget {
    Module(CallableId),
    External(ExternalCallableId),
}

/// One exact function override selected in Pass 1. All types are Kotlin semantic types; the
/// declaration-side shape remains unapplied so each backend can perform its own erasure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFunctionOverride {
    pub implementation: ResolvedFunctionOverrideTarget,
    pub implementation_owner: TypeName,
    pub overridden: ResolvedFunctionOverrideTarget,
    pub overridden_owner: TypeName,
    /// The overridden owner is an interface. This is a semantic classifier fact, retained so a
    /// representation backend does not re-query the declaration provider merely to distinguish
    /// superclass and interface dispatch obligations.
    pub overridden_is_interface: bool,
    /// Semantic declaration name. Physical spellings remain behind external callable identities and
    /// are selected only by a target backend.
    pub name: Box<str>,
    pub declared_parameters: Box<[ResolvedTy]>,
    pub declared_result: ResolvedTy,
    pub applied_parameters: Box<[ResolvedTy]>,
    pub applied_result: ResolvedTy,
    /// Declaration-side implementation shape before applying the inheriting class's type arguments.
    /// The frontend has already selected the declaration; a backend only erases this shape.
    pub implementation_parameters: Box<[ResolvedTy]>,
    pub implementation_result: ResolvedTy,
    pub suspend: bool,
    pub depth: u32,
}
