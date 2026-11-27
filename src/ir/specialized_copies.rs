//! The records that say which inline expansion copied a lambda implementation or an anonymous
//! class into a call site, and the inline callee whose body it copied.

use super::{ClassId, ExprId, FunId, IrEnclosure};
use crate::types::Ty;

/// The inline function whose body an expansion copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IrInlineCallee {
    /// A same-module inline callable, by its stable checked identity.
    Module(crate::fir::CallableId),
    /// An inline dependency function, expanded from its template in this file.
    Dependency(crate::fir::ExternalCallableId),
}

/// A call-site copy of a lambda implementation.
///
/// The record keeps the checked declaration identities and lexical containment that caused the
/// copy. Source spellings are retained only as rendering provenance. A backend decides whether the
/// copy needs a separate artifact and owns every physical name and ordinal used for that artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrSpecializedFunction {
    pub source: FunId,
    /// Stable declaration whose body contains the expansion.
    pub caller_declaration: crate::fir::DeclarationId,
    /// Exact executable scope containing the expansion. `None` means no declaration identity was
    /// available for the fragment.
    pub caller: Option<IrEnclosure>,
    /// The expansion is evaluated in a callable's default-argument fragment. The caller still
    /// records that callable or constructor identity; a target owns the physical `$default` or
    /// constructor-special rendering.
    pub caller_is_default: bool,
    /// Source spelling of that declaration, or the deliberately empty spelling of a constructor or
    /// unnamed initializer. This is rendering provenance, never the expansion identity.
    pub caller_source_name: String,
    /// The inline callable that was expanded.
    pub inline_callee: IrInlineCallee,
    /// Source spelling retained for target-specific debug/artifact rendering.
    pub inline_callee_source_name: String,
    /// The expanded call omits an argument that has a default, so it is the callee's default entry
    /// that the expansion copies. A target owns that entry's physical rendering (`$default`).
    pub inline_callee_default_entry: bool,
    /// Specialized implementation lexically containing this one. Nested copies retain containment
    /// rather than being flattened into another top-level expansion ordinal.
    pub parent: Option<FunId>,
    /// This copy consumed a supplied inline-lambda capture into its implementation. Even when the
    /// supplied lambda had no captures and the resulting closure has no fields, each expansion
    /// still creates that closure value rather than sharing the declaration's singleton.
    pub consumed_inline_capture: bool,
}

/// A call-site copy of an anonymous class whose members use a reified type parameter.
///
/// The declaration class stays in place for the inline method's own body. This record is the
/// expansion that copied it. A backend owns the physical `{owner}${caller}$$inlined${callee}$N`
/// name and shares that ordinal sequence with a specialized lambda of the same expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrSpecializedAnonymousClass {
    pub source: ClassId,
    /// Expression identity of the copied construction, so the copy sorts with a specialized lambda
    /// of the same expansion.
    pub order: u32,
    pub caller_declaration: crate::fir::DeclarationId,
    pub caller: Option<IrEnclosure>,
    pub caller_is_default: bool,
    pub caller_source_name: String,
    pub inline_callee: IrInlineCallee,
    pub inline_callee_source_name: String,
    /// The expanded call goes through the callee's default entry; see
    /// [`IrSpecializedFunction::inline_callee_default_entry`].
    pub inline_callee_default_entry: bool,
    /// For each of the callee's operands, by its value slot: the caller's `crossinline` parameter
    /// the expansion binds it to, when it is one. A copy remaps its captures' provenance through
    /// it, and so does an object nested in the copy that is specialized later.
    pub crossinline_operands: Vec<Option<u32>>,
    /// The call-site copy whose body regenerated this copy, when the object is nested in another
    /// regenerated anonymous object. A target names it under that copy rather than as another
    /// expansion of the inline call.
    pub parent: Option<ClassId>,
    /// Source function → the function on this copy. Accessor functions materialized after the
    /// copy are added here before bridges and metadata read the copy.
    pub method_clones: crate::fx_hash::HashMap<FunId, FunId>,
    /// Call-site substitutions. Property accessors are materialized after this copy, and their
    /// bodies are specialized with these bindings.
    pub bindings: crate::fx_hash::HashMap<String, Ty>,
    pub reified_bindings: crate::fx_hash::HashMap<String, Ty>,
    /// Fields and properties already on the declaration class when this copy was taken.
    /// Later materialization appends backing fields and accessors past these prefixes.
    pub field_count: u32,
    pub property_count: u32,
    /// Capture fields removed from this copy because their `crossinline` lambda was inlined into
    /// the copy's methods. `field_count` still counts them on the declaration, so later fields
    /// appended to the declaration stay past that prefix.
    pub omitted_capture_fields: u32,
    /// For each constructor parameter of the copy, the declaration's parameter it was copied from,
    /// or `None` for a value the copy keeps in place of an inlined lambda. Constructor code the
    /// copy takes from the declaration once lowering has realized it (the superclass constructor
    /// call and the initializer) reads its parameters through this.
    pub constructor_parameters: Vec<Option<u32>>,
    /// Property reads and writes in the inlined caller whose receiver is this copy's construction.
    /// Accessor functions are not available when the construction is retargeted, so the read is
    /// rebound once the copy's properties exist.
    pub caller_property_uses: Vec<ExprId>,
    /// Cloned property initializer and accessor roots. Accessor functions do not exist yet, and
    /// these roots are not constructor statements. Nested specialization walks this field;
    /// [`IrClass::init_body`] stays the constructor body.
    pub pending_property_roots: Vec<ExprId>,
}
