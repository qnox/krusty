//! Checked declaration identities used by the shared class table mechanism.

use crate::fir::{
    CallableId, ExternalCallableId, PropertyId, ResolvedFunctionOverrideTarget,
    ResolvedPropertyOverrideTarget,
};
use crate::ir::FunId;
use crate::types::TypeName;

/// A member's identity in a planned table, the same in every file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlanKey {
    Callable(CallableId),
    /// A body-local implementation function in the current IR file. This identity is never
    /// published or reused by another file.
    FileFunction(FunId),
    /// A current-file property getter. The semantic property identity remains separate from its
    /// file-local accessor body.
    FileGetter {
        property: ResolvedPropertyOverrideTarget,
        function: FunId,
    },
    /// A current-file property setter, kept distinct from its getter and property declaration.
    FileSetter {
        property: ResolvedPropertyOverrideTarget,
        function: FunId,
    },
    Getter(PropertyId),
    Setter(PropertyId),
    /// The `ordinal`-th forwarder `class`'s interface delegations generate.
    Forwarder {
        class: TypeName,
        ordinal: u32,
    },
    /// A member of a dependency class a module class extends, by its provider identity. A
    /// dependency property is keyed by its getter's identity.
    DependencyCallable(ExternalCallableId),
    DependencyGetter(ExternalCallableId),
    DependencySetter(ExternalCallableId),
}

impl PlanKey {
    /// The key of the function `callable`.
    pub(crate) fn function(callable: ResolvedFunctionOverrideTarget) -> Self {
        match callable {
            ResolvedFunctionOverrideTarget::Module(callable) => Self::Callable(callable),
            ResolvedFunctionOverrideTarget::External(callable) => {
                Self::DependencyCallable(callable)
            }
        }
    }

    /// The key of the getter, or with `setter` the setter, of `property`.
    pub(crate) fn accessor(property: ResolvedPropertyOverrideTarget, setter: bool) -> Self {
        match (property, setter) {
            (ResolvedPropertyOverrideTarget::Module(property), false) => Self::Getter(property),
            (ResolvedPropertyOverrideTarget::Module(property), true) => Self::Setter(property),
            (ResolvedPropertyOverrideTarget::External(property), false) => {
                Self::DependencyGetter(property)
            }
            (ResolvedPropertyOverrideTarget::External(property), true) => {
                Self::DependencySetter(property)
            }
        }
    }
}
