//! Bridge methods: the declaration adapters a supertype's erased signature makes necessary.
//!
//! A bridge carries the supertype's signature and delegates to the concrete override, adapting both
//! ends. Everything a target needs to decide those adaptations lives here, beside the declaration
//! it adapts, rather than in the file arena that merely holds them.

use super::Ty;
use crate::fir::ResolvedParameterIdentity;

/// One parameter of a bridge, described by the OVERRIDDEN declaration the bridge's signature comes
/// from: kotlinc names and labels a bridge's locals after that declaration, never after the
/// override it delegates to, whose parameter types a representation pass may later rewrite into
/// physical carriers.
#[derive(Clone, Debug, PartialEq)]
pub struct BridgeParameter {
    pub identity: ResolvedParameterIdentity,
    /// The overridden declaration's semantic type for this parameter, before erasure.
    pub semantic: Ty,
}

/// A JVM declaration adapter (`name(erased_params)erased_ret` → a selected concrete target).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeKind {
    Function,
    PropertyGetter,
    PropertySetter,
    /// The ordinary public instance entry through which a boxed value class implements an interface;
    /// it delegates to the selected static carrier implementation but is not `ACC_BRIDGE|SYNTHETIC`.
    ValueClassInterfaceEntry,
}

#[derive(Clone, Debug)]
pub struct Bridge {
    pub kind: BridgeKind,
    /// Exact same-module function this bridge delegates to. Backend realization uses this stable
    /// identity for representation decisions; `target_name` is emitted spelling, never lookup input.
    pub target_function: Option<u32>,
    /// The parameters visible on this bridge, one per erased parameter. These are frozen before a
    /// representation pass can prepend carrier/receiver parameters to the delegated target.
    pub parameters: Vec<BridgeParameter>,
    pub name: String,
    pub erased_params: Vec<Ty>,
    pub erased_ret: Ty,
    pub concrete_params: Vec<Ty>,
    pub concrete_ret: Ty,
    /// Physical return in the delegated target's descriptor when it differs from the value the bridge
    /// adapts. A suspend target always returns `Object`; a generic bridge may still need to cast that
    /// object to a reference carrier and box it as a value class for the erased supertype boundary.
    pub target_ret: Option<Ty>,
    /// Whether incompatible erased arguments return the collection operation's neutral result.
    pub type_safe_barrier: bool,
    /// kotlinc's `BRIDGE_SPECIAL`: the bridge a builtin member gets under the JVM name it maps to
    /// (`size()` for `Collection.size`). It is final and not synthetic, so a subclass inherits it
    /// rather than declaring it again.
    pub special: bool,
    /// The method this bridge delegates to, when it differs from `name` — a value-class-returning
    /// override is emitted under a mangled name (`foo-<hash>`), so the unmangled bridge (`foo`, the
    /// supertype's erased signature) must call the mangled one. `None` ⇒ same as `name`.
    pub target_name: Option<String>,
}
