//! Bridge methods: the declaration adapters a supertype's erased signature makes necessary.
//!
//! A bridge carries the supertype's signature and delegates to the concrete override, adapting both
//! ends. Everything a target needs to decide those adaptations lives here, beside the declaration
//! it adapts, rather than in the file arena that merely holds them.

use super::{Ty, TypeName};

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
    /// The method this bridge delegates to, when it differs from `name` — a value-class-returning
    /// override is emitted under a mangled name (`foo-<hash>`), so the unmangled bridge (`foo`, the
    /// supertype's erased signature) must call the mangled one. `None` ⇒ same as `name`.
    pub target_name: Option<String>,
    /// When set, the bridge boxes its (unboxed value-class) result with `<owner>.box-impl` before
    /// returning — a value-class-returning override seen through a supertype hands back a boxed `X`.
    pub box_ret: Option<TypeName>,
    /// Per concrete parameter, the boxed value class to `checkcast` + `unbox-impl` before the target
    /// call — a generic supertype method (`B.f(T,U)` → erased `f(Object,Object)`) delegates to a
    /// mangled concrete override taking the value class's UNDERLYING, while the incoming arg is a
    /// boxed `X`. Empty (or all-`None`) ⇒ plain checkcast/convert (the common case). JVM/value-class
    /// concern, populated by the value-class pass; the front end leaves it empty.
    pub unbox_params: Vec<Option<TypeName>>,
}
