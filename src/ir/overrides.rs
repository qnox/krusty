//! Exact override edges selected by the frontend, as common IR carries them to the backends.
//!
//! Each edge joins an implementation to the declaration it overrides by stable identity, with the
//! declaration-side and implementation-side types a target erases for its bridges. A backend
//! consumes these edges; it never re-derives an override by matching names.

use super::{FunId, Ty, TypeName};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrPropertyOverride {
    pub implementation: crate::fir::ResolvedPropertyOverrideTarget,
    /// Common-IR accessors of a compiler-generated implementation (see `IrFunctionOverride`).
    pub implementation_getter: Option<FunId>,
    pub implementation_setter: Option<FunId>,
    pub implementation_owner: TypeName,
    pub overridden: crate::fir::ResolvedPropertyOverrideTarget,
    pub overridden_owner: TypeName,
    pub overridden_is_interface: bool,
    pub name: String,
    pub declared_type: Ty,
    pub applied_type: Ty,
    pub implementation_type: Ty,
    /// Member-extension receivers, as on `ResolvedPropertyOverride`.
    pub declared_receiver: Option<Ty>,
    pub implementation_receiver: Option<Ty>,
    pub overridden_mutable: bool,
    pub implementation_mutable: bool,
    pub has_kotlin_superclass_override: bool,
    pub depth: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrFunctionOverride {
    pub implementation: crate::fir::ResolvedFunctionOverrideTarget,
    /// Exact common-IR implementation for a compiler-generated declaration such as an interface
    /// delegation forwarder. Source overrides use their stable callable identity and leave this
    /// empty; backends consume either edge without matching a method by name.
    pub implementation_function: Option<FunId>,
    pub implementation_owner: TypeName,
    pub overridden: crate::fir::ResolvedFunctionOverrideTarget,
    pub overridden_owner: TypeName,
    pub overridden_is_interface: bool,
    pub name: String,
    pub declared_parameters: Vec<Ty>,
    pub declared_result: Ty,
    pub applied_parameters: Vec<Ty>,
    pub applied_result: Ty,
    pub implementation_parameters: Vec<Ty>,
    pub implementation_parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    pub implementation_result: Ty,
    pub suspend: bool,
    pub has_kotlin_superclass_override: bool,
    pub depth: u32,
}
