//! Package-level declaration records copied from finalized headers, plus the file's selected
//! program entry point. These are backend-neutral Kotlin declaration facts: a target formats
//! metadata or chooses an ABI from them without reopening the frontend index.

use super::FunId;
pub use crate::program_entry::MainEntryParameters;
use crate::types::{Ty, TypeName};

/// The Kotlin `main` a file declares, as the frontend selected it (`fir::ResolvedEntryPoint`) and
/// common lowering realized it in this file's function arena. A backend that produces a program
/// starts here; it never looks for a function by its spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrEntryPoint {
    pub function: FunId,
    pub parameters: MainEntryParameters,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrPackageTypeParameter {
    pub name: String,
    pub semantic_name: String,
    pub bounds: Vec<Ty>,
    pub reified: bool,
}

/// Backend-neutral package-function declaration metadata. `function` is the exact common-IR
/// realization; all remaining fields describe the Kotlin declaration before target erasure.
#[derive(Clone, Debug, PartialEq)]
pub struct IrPackageFunction {
    pub function: FunId,
    pub name: String,
    pub params: Vec<(String, Ty)>,
    pub ret: Ty,
    pub receiver: Option<Ty>,
    pub param_defaults: Vec<bool>,
    pub suspend: bool,
    pub inline: bool,
    pub operator: bool,
    pub infix: bool,
    pub tailrec: bool,
    /// A value parameter (context parameters excluded) or the extension receiver has a function type.
    pub has_function_typed_parameter: bool,
    /// `companion fun C.name`: the receiver is a lookup coordinate, absent from the JVM method.
    pub companion: bool,
    pub contract: Option<crate::contracts::ResolvedContract>,
    pub type_params: Vec<IrPackageTypeParameter>,
    pub context_count: usize,
    pub vararg_index: Option<usize>,
    pub visibility: crate::types::Visibility,
    pub spellings: crate::spelling::DeclaredSpellings,
    pub source_order: u32,
    /// Byte range of the source signature, starting at `fun`. The JVM clash diagnostic points here
    /// after representation has chosen the physical method name and descriptor.
    pub signature_span: crate::diag::Span,
}

/// Backend-neutral package-property declaration metadata. The checked property table retains its
/// exact stable identity while bodies stream; this compact record is what survives into backend
/// metadata formatting after common lowering has finished.
#[derive(Clone, Debug, PartialEq)]
pub struct IrPackageProperty {
    /// Stable semantic property identity. This joins the declaration header to the common-IR
    /// layout selected while its checked body streamed; it is not a source coordinate or a target
    /// storage identity.
    pub property: crate::fir::PropertyId,
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub type_params: Vec<IrPackageTypeParameter>,
    pub receiver: Option<Ty>,
    pub context_parameters: Vec<Ty>,
    pub context_parameter_names: Vec<String>,
    pub context_parameter_kinds: Vec<crate::types::ContextParameterKind>,
    pub is_const: bool,
    pub has_constant: bool,
    pub visibility: crate::types::Visibility,
    /// Resolved Kotlin annotation identities. Backends interpret annotations in their own
    /// namespace; common lowering does not turn them into storage or calling-convention choices.
    pub annotations: Box<[TypeName]>,
    /// Final Kotlin declaration modifiers copied from the stable header. Representation passes may
    /// inspect these semantic restrictions without reopening FIR or recovering a declaration by
    /// spelling.
    pub flags: crate::fir::DeclarationFlags,
    pub spellings: crate::spelling::DeclaredSpellings,
    pub has_backing_field: bool,
    /// How the accessors are declared: source-written, delegated, or the compiler default.
    pub modifiers: super::IrPropertyModifiers,
    /// The setter's own visibility: its declaration's (`internal set`, `private set`), else the
    /// property's.
    pub setter_visibility: crate::types::Visibility,
    pub source_order: u32,
}
