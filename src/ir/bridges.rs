//! Bridge methods: the declaration adapters a supertype's erased signature makes necessary.
//!
//! A bridge carries the supertype's signature and delegates to the concrete override, adapting both
//! ends. Everything a target needs to decide those adaptations lives here, beside the declaration
//! it adapts, rather than in the file arena that merely holds them.

use super::{Ty, TypeName};
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

/// The collection-bridge guard a JVM pass selects once: which erased parameters to test, in order,
/// how, and the neutral result to return when a value is not its parameter's type. Common lowering
/// leaves the plan absent. Emission writes this record and does not select it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionBarrierPlan {
    pub parameters: Vec<CollectionBarrierParameter>,
    pub outcome: crate::libraries::CollectionBarrierOutcome,
}

/// One erased parameter a collection guard tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectionBarrierParameter {
    pub parameter: usize,
    pub check: CollectionBarrierCheck,
}

/// How a collection guard tests one parameter against the override's type for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionBarrierCheck {
    /// The type's JVM class is narrower than `Object`: an `instanceof` test, which `null` passes
    /// when the type admits it.
    Instance { admits_null: bool },
    /// The type erases to `Object`: only `null` can fail it, when the type rejects `null`.
    Presence { admits_null: bool },
}

/// Which accessor of a property a bridge delegates to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeAccessorRole {
    Getter,
    Setter,
}

/// The same-module property a property-accessor bridge delegates to.
///
/// This is the override edge's selected implementation, not a JVM spelling. Retargeting matches
/// it through the property's layout; `Bridge::name` and `Bridge::target_name` stay emission text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgePropertyImplementation {
    pub property: crate::fir::PropertyId,
    pub accessor: BridgeAccessorRole,
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
    /// Semantic owner of the exact declaration this bridge overrides. Generated adapters that do
    /// not come from an override edge leave this absent. A backend may attach ABI policy to this
    /// identity without recovering the declaration from `name`.
    pub overridden_owner: Option<TypeName>,
    /// Provider-normalized collection ABI role of the exact overridden declaration.
    pub collection_barrier: Option<crate::libraries::CollectionBarrier>,
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
    /// Collection guard selected for this bridge. Absent until the JVM pass records one.
    pub barrier_plan: Option<CollectionBarrierPlan>,
    /// kotlinc's `BRIDGE_SPECIAL`: the bridge a builtin member gets under the JVM name it maps to
    /// (`size()` for `Collection.size`) or under a type-safe collection member's erased root
    /// signature (`contains(Object)`). It is final, so a subclass inherits it rather than declaring
    /// it again. Unlike an ordinary erasure bridge it is not synthetic.
    pub special: bool,
    /// This bridge publishes an `internal` member's JVM slot. Its name still matches the
    /// implementation when it is recorded, because the module suffix is applied later; the
    /// value-class pass must keep it instead of dropping it as a duplicate of that method.
    pub module_name_bridge: bool,
    /// The method this bridge delegates to, when it differs from `name` — a value-class-returning
    /// override is emitted under a mangled name (`foo-<hash>`), so the unmangled bridge (`foo`, the
    /// supertype's erased signature) must call the mangled one. `None` ⇒ same as `name`.
    pub target_name: Option<String>,
    /// Selected implementation of a property-accessor bridge. Function bridges and external
    /// implementations leave this absent.
    pub property_implementation: Option<BridgePropertyImplementation>,
}
