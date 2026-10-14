//! Common-IR contracts owned by enum declarations and enum-switch lowering.

use super::{ClassId, ExprId, IrConstructorTarget};
use crate::types::{Ty, TypeName};

#[derive(Clone, Debug)]
pub struct IrCheckedEnumEntryBody {
    pub declaration: crate::fir::DeclarationId,
    pub class: ClassId,
    pub ordinal: u32,
    pub name: String,
    pub construction: ExprId,
}

/// One claim on the emitting class's `$WhenMappings`: the `$EnumSwitchMapping$N` table for
/// `classifier`, and, with `entry`, that entry's key in it. A table is numbered by the first claim
/// on its enum and a key by the first claim on its entry, so claims are replayed in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumSwitchRegistration {
    pub classifier: TypeName,
    pub entry: Option<Box<str>>,
}

/// One entry of an enum class. Groups what were parallel `Vec`s keyed by entry index (the
/// `(name, args)` tuple plus the separate `subclass` vec), so an entry's name, constructor
/// arguments, and synthesized-subclass marker cannot desynchronize.
#[derive(Clone, Debug)]
pub struct IrEnumEntry {
    /// Entry name (`RED`).
    pub name: String,
    /// Exact source span of the entry declaration for diagnostics about its physical surface.
    /// Dependency and synthesized entries have no source location.
    pub diagnostic_span: Option<crate::diag::Span>,
    /// Checked source-order evaluation that must run before the physical constructor is entered.
    pub argument_prelude: Vec<ExprId>,
    /// Lowered constructor-argument values; empty for an argument-less entry.
    pub args: Vec<ExprId>,
    /// Semantic parameter list of the constructor selected for this entry.
    pub constructor_parameter_types: Vec<Ty>,
    /// Exact constructor selected by checked FIR. A backend consumes this declaration ordinal.
    pub constructor: IrConstructorTarget,
    /// Selected constructor parameters whose values come from declaration defaults.
    pub default_parameters: Vec<u32>,
    /// One-based source line of the entry declaration for emitted debug information.
    pub decl_line: u32,
    /// Stable source position among the class's members.
    pub source_order: u32,
    /// Synthesized anonymous subclass for an entry with a body.
    pub subclass: Option<TypeName>,
}
