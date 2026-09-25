//! Stable override decisions published while the declaration providers are live.
//!
//! An override edge is a Kotlin semantic fact. It records the exact declarations and the raw and
//! applied language types; it does not record a JVM descriptor, erased storage type, accessor
//! spelling, or bridge method. A backend may use the edge to decide whether its representation
//! requires a bridge, but it must not repeat property lookup or infer an override from a name.

use super::{
    CallableId, DeclarationId, DeclarationKind, ExternalCallableId, PropertyId,
    ResolvedModuleIndex, ResolvedTy,
};
use crate::types::TypeName;

/// Source identity or semantic compiler-generated role of a callable parameter. Target backends
/// format generated roles for their ABI; common phases never invent a platform spelling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedParameterIdentity {
    Source(Box<str>),
    /// The declaration/provider publishes no source name for this parameter. The ordinal is stable
    /// semantic position only; targets must not turn it into a fabricated `pN` spelling.
    Unnamed {
        ordinal: u32,
    },
    ContextValue {
        ordinal: u32,
        source_name: Box<str>,
    },
    AnonymousContextParameter {
        ordinal: u32,
    },
    LegacyContextReceiver {
        ordinal: u32,
    },
    ExtensionReceiver,
    PropertySetterValue,
    SuspendCompletion,
}

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
    /// A member extension's declared receiver on each side (`val T.x` over `val String.x`); `None`
    /// for a member property. Like the types, a target backend erases these for its accessors.
    pub declared_receiver: Option<ResolvedTy>,
    pub implementation_receiver: Option<ResolvedTy>,
    pub overridden_mutable: bool,
    pub implementation_mutable: bool,
    /// Return-value status the overridden declaration's provider recorded. `None` for a
    /// current-module declaration, whose status is derived from its own edges, and for a Java
    /// declaration, which records none; see [`crate::libraries::LibraryMember::return_value_status`].
    pub overridden_return_value_status: Option<crate::types::ReturnValueStatus>,
    /// Whether a Kotlin superclass declaration among the implementation's other overridden
    /// properties itself overrides `overridden`. A target realization of `overridden` (such as a
    /// JVM renamed-builtin bridge) may therefore already be owned by that superclass.
    pub has_kotlin_superclass_override: bool,
    pub depth: u32,
}

/// Declaration status a function takes from the declarations it overrides, as kotlinc's status
/// resolution derives it: the return-value status of the first overridden declaration that records
/// one, and the `operator` / `infix` modifiers when any overridden declaration has them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InheritedCallableStatus {
    pub return_value: crate::types::ReturnValueStatus,
    pub operator: bool,
    pub infix: bool,
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
    pub implementation_parameter_identities: Box<[ResolvedParameterIdentity]>,
    pub implementation_result: ResolvedTy,
    /// Default availability exposed by the overridden declaration at this exact applied edge.
    /// Kept beside the stable provider identity so module-to-dependency inheritance never has to
    /// reopen metadata after the override graph is frozen.
    pub overridden_parameter_defaults: Box<[bool]>,
    pub overridden_default_provider: Option<ResolvedFunctionOverrideTarget>,
    /// See [`ResolvedPropertyOverride::overridden_return_value_status`].
    pub overridden_return_value_status: Option<crate::types::ReturnValueStatus>,
    /// Whether the overridden declaration is an `operator` / `infix` function as its provider
    /// records it. An override inherits both modifiers, so its own metadata repeats them.
    pub overridden_operator: bool,
    pub overridden_infix: bool,
    pub suspend: bool,
    /// Whether a Kotlin superclass declaration among the implementation's other overridden
    /// functions itself overrides `overridden`; see
    /// [`ResolvedPropertyOverride::has_kotlin_superclass_override`].
    pub has_kotlin_superclass_override: bool,
    pub depth: u32,
}

impl ResolvedModuleIndex {
    pub fn property_overrides(&self, classifier: DeclarationId) -> &[ResolvedPropertyOverride] {
        self.property_overrides
            .get(&classifier)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(crate) fn has_property_override_plan(&self, classifier: DeclarationId) -> bool {
        self.property_overrides.contains_key(&classifier)
    }

    pub(crate) fn publish_property_overrides(
        &mut self,
        owner: DeclarationId,
        overrides: impl IntoIterator<Item = ResolvedPropertyOverride>,
    ) {
        assert!(
            self.classifier_header(owner).is_some()
                || self
                    .declaration_header(owner)
                    .is_some_and(|header| header.kind == DeclarationKind::EnumEntry),
            "property overrides require a published classifier or enum-entry owner"
        );
        let overrides = overrides.into_iter().collect::<Vec<_>>().into_boxed_slice();
        assert!(
            self.property_overrides.insert(owner, overrides).is_none(),
            "a source classifier or enum entry may publish property overrides only once"
        );
    }

    pub fn function_overrides(&self, classifier: DeclarationId) -> &[ResolvedFunctionOverride] {
        self.function_overrides
            .get(&classifier)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(crate) fn has_function_override_plan(&self, classifier: DeclarationId) -> bool {
        self.function_overrides.contains_key(&classifier)
    }

    pub(crate) fn publish_function_overrides(
        &mut self,
        owner: DeclarationId,
        overrides: impl IntoIterator<Item = ResolvedFunctionOverride>,
    ) {
        assert!(
            self.classifier_header(owner).is_some()
                || self
                    .declaration_header(owner)
                    .is_some_and(|header| header.kind == DeclarationKind::EnumEntry),
            "function overrides require a published classifier or enum-entry owner"
        );
        let overrides = overrides.into_iter().collect::<Vec<_>>().into_boxed_slice();
        assert!(
            self.function_overrides.insert(owner, overrides).is_none(),
            "a source classifier or enum entry may publish function overrides only once"
        );
    }

    /// What a current-module function inherits from the declarations it overrides.
    pub fn callable_inherited_status(&self, callable: CallableId) -> InheritedCallableStatus {
        self.callable_inherited_statuses
            .get(&callable)
            .copied()
            .unwrap_or_default()
    }

    pub fn property_return_value_status(
        &self,
        property: PropertyId,
    ) -> crate::types::ReturnValueStatus {
        self.property_return_value_statuses
            .get(&property)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn publish_callable_inherited_status(
        &mut self,
        callable: CallableId,
        status: InheritedCallableStatus,
    ) {
        if status != InheritedCallableStatus::default() {
            assert!(
                self.callable_inherited_statuses
                    .insert(callable, status)
                    .is_none(),
                "a callable may publish its inherited status only once"
            );
        }
    }

    pub(crate) fn publish_property_return_value_status(
        &mut self,
        property: PropertyId,
        status: crate::types::ReturnValueStatus,
    ) {
        if status != crate::types::ReturnValueStatus::Unspecified {
            assert!(
                self.property_return_value_statuses
                    .insert(property, status)
                    .is_none(),
                "a property may publish its return-value status only once"
            );
        }
    }
}
