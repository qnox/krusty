//! Stable checked contract for `Interface by value` forwarding.
//!
//! Resolution owns this plan. Common lowering materializes it, and backends consume its exact
//! callable identities and applied signatures without reopening a scope or provider hierarchy.

use crate::types::TypeName;

use super::{
    CallableId, ExternalCallableId, PropertyId, ResolvedFunctionOverrideTarget,
    ResolvedParameterIdentity, ResolvedPropertyOverrideTarget, ResolvedTy,
};

/// Exact checked runtime source for the value stored in one interface-delegate field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedInterfaceDelegateSource {
    /// Declared primary-constructor parameter ordinal. Common lowering adds the already-materialized
    /// compiler prefix before reading the physical constructor value.
    ConstructorParameter(u32),
    /// Declared primary-constructor parameter ordinal whose read-only property's backing field is
    /// the delegate, as kotlinc realizes it: no separate delegate field is synthesized.
    ConstructorProperty(u32),
    /// Exact physical ordinal in the compiler-supplied constructor prefix. Pass 2 fixes this after
    /// anonymous-object capture discovery and carries the checked value on the construction FIR.
    SyntheticConstructorParameter(u32),
    /// The checked primary-constructor FIR contains an initializer statement at this delegation's
    /// stable ordinal. Lowering records the resulting field coordinate while consuming that body.
    ConstructorBodyInitializer,
}

/// One interface-delegation edge after semantic resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedInterfaceDelegation {
    pub interface: ResolvedTy,
    pub source: ResolvedInterfaceDelegateSource,
    /// Exact forwarding declaration order selected in Pass 1. Function/property interleaving is a
    /// declaration fact and cannot be reconstructed from separate lowering loops.
    pub members: Box<[ResolvedDelegatedMember]>,
}

/// Stable identity of one already-selected delegated accessor/function invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedDelegatedModuleTarget {
    Function(CallableId),
    PropertyGetter(PropertyId),
    PropertySetter(PropertyId),
}

/// Provider-neutral realization of an already-selected delegated call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedDelegatedCallTarget {
    Module {
        target: ResolvedDelegatedModuleTarget,
        owner: TypeName,
        name: Box<str>,
        parameters: Box<[ResolvedTy]>,
        result: ResolvedTy,
        interface: bool,
    },
    External(ExternalCallableId),
}

/// Complete checked call shape used by one generated delegation forwarder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedCall {
    pub target: ResolvedDelegatedCallTarget,
    pub receiver: ResolvedTy,
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    pub declared_result: Option<ResolvedTy>,
    pub suspend: bool,
    /// Semantic parameter occupied by a member-extension receiver. It is already part of
    /// `parameters`; the target backend only needs to distinguish it from a value parameter.
    pub extension_receiver_parameter: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedFunction {
    pub name: Box<str>,
    /// Parameter identities owned by the generated forwarding declaration.
    pub parameter_identities: Box<[ResolvedParameterIdentity]>,
    /// Function-owned generic parameters of the selected interface declaration.
    pub type_parameters: Box<[ResolvedDelegatedTypeParameter]>,
    /// Every exact interface declaration the one generated body implements.
    pub overridden: Box<[ResolvedDelegatedFunctionDeclaration]>,
    pub call: ResolvedDelegatedCall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedTypeParameter {
    pub name: Box<str>,
    pub semantic_name: Box<str>,
    pub bounds: Box<[ResolvedTy]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedFunctionDeclaration {
    pub target: ResolvedFunctionOverrideTarget,
    pub owner: TypeName,
    pub semantic_role: Option<crate::types::SemanticCallRole>,
    pub collection_barrier: Option<crate::libraries::CollectionBarrierOutcome>,
    pub parameter_identities: Box<[ResolvedParameterIdentity]>,
    /// The declaration's own unapplied semantic signature.
    pub parameters: Box<[ResolvedTy]>,
    pub result: ResolvedTy,
    /// The declaration as viewed through the delegating classifier's applied hierarchy.
    pub applied_parameters: Box<[ResolvedTy]>,
    pub applied_result: ResolvedTy,
    pub interface: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedDelegatedMember {
    Function(ResolvedDelegatedFunction),
    Property(ResolvedDelegatedProperty),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedProperty {
    pub name: Box<str>,
    /// A member-extension property's own type parameters (`val <T> T.id: T`).
    pub type_parameters: Box<[ResolvedDelegatedTypeParameter]>,
    pub ty: ResolvedTy,
    pub context_parameters: Box<[ResolvedDelegatedContextParameter]>,
    pub getter: ResolvedDelegatedCall,
    pub setter: Option<ResolvedDelegatedCall>,
    /// Exact declarations the generated accessors override. The delegated interface declaration
    /// comes first; a concrete inherited class property follows when Kotlin delegation replaces
    /// that superclass slot (KT-70417).
    pub overridden: Box<[ResolvedDelegatedPropertyDeclaration]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedPropertyDeclaration {
    pub target: ResolvedPropertyOverrideTarget,
    pub owner: TypeName,
    /// Unsubstituted declared type, the one the overridden accessors are erased from.
    pub ty: ResolvedTy,
    /// The declaration as viewed through the delegating classifier's applied hierarchy.
    pub applied_ty: ResolvedTy,
    /// Unsubstituted declared receiver of a member-extension property, erased likewise.
    pub receiver: Option<ResolvedTy>,
    pub mutable: bool,
    pub interface: bool,
    pub depth: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDelegatedContextParameter {
    pub name: Box<str>,
    pub kind: crate::types::ContextParameterKind,
    pub ty: ResolvedTy,
}

impl ResolvedDelegatedCall {
    fn storage_payload_bytes(&self) -> usize {
        self.parameters.len() * std::mem::size_of::<ResolvedTy>()
            + match &self.target {
                ResolvedDelegatedCallTarget::Module {
                    name, parameters, ..
                } => name.len() + parameters.len() * std::mem::size_of::<ResolvedTy>(),
                ResolvedDelegatedCallTarget::External(_) => 0,
            }
    }
}

fn type_parameters_payload_bytes(parameters: &[ResolvedDelegatedTypeParameter]) -> usize {
    std::mem::size_of_val(parameters)
        + parameters
            .iter()
            .map(|parameter| {
                parameter.name.len()
                    + parameter.semantic_name.len()
                    + parameter.bounds.len() * std::mem::size_of::<ResolvedTy>()
            })
            .sum::<usize>()
}

impl ResolvedInterfaceDelegation {
    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.members.len() * std::mem::size_of::<ResolvedDelegatedMember>()
            + self
                .members
                .iter()
                .map(|member| match member {
                    ResolvedDelegatedMember::Function(function) => {
                        function.name.len()
                            + type_parameters_payload_bytes(&function.type_parameters)
                            + function
                                .overridden
                                .iter()
                                .map(|declaration| {
                                    std::mem::size_of::<ResolvedDelegatedFunctionDeclaration>()
                                        + (declaration.parameters.len()
                                            + declaration.applied_parameters.len())
                                            * std::mem::size_of::<ResolvedTy>()
                                })
                                .sum::<usize>()
                            + function.call.storage_payload_bytes()
                    }
                    ResolvedDelegatedMember::Property(property) => {
                        property.name.len()
                            + type_parameters_payload_bytes(&property.type_parameters)
                            + property.overridden.len()
                                * std::mem::size_of::<ResolvedDelegatedPropertyDeclaration>()
                            + property.context_parameters.len()
                                * std::mem::size_of::<ResolvedDelegatedContextParameter>()
                            + property
                                .context_parameters
                                .iter()
                                .map(|parameter| parameter.name.len())
                                .sum::<usize>()
                            + property.getter.storage_payload_bytes()
                            + property
                                .setter
                                .as_ref()
                                .map(ResolvedDelegatedCall::storage_payload_bytes)
                                .unwrap_or_default()
                    }
                })
                .sum::<usize>()
    }
}
