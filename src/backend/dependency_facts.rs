//! Frozen facts about the dependency callables one checked file's IR selected.
//!
//! Checked IR names a dependency callable or property only by the opaque identity its provider
//! assigned. This table copies, once per file at the frontend/backend boundary, what that provider
//! normalized for exactly the identities the file's IR holds, so a backend can read a dependency
//! callable's or property's facts without asking the provider about it while it emits. It covers
//! callables and properties only; dependency classifiers still reach a backend through
//! `CheckedBackendClassifiers`, which asks the provider. It answers only for an identity the IR
//! already holds: there is no lookup by name, owner, or signature.

mod references;

#[cfg(test)]
pub(crate) use references::referenced_dependencies;

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use crate::fir::{ExternalCallableId, ExternalPropertyId};
use crate::libraries::{
    CompilerIntrinsic, DefaultCallRealization, ExternalCallableKind, GenericSig, InlineKind,
    MemberRealization, NonvirtualCallRealization, SemanticCallRole,
};
use crate::symbol_source::SymbolSource;
use crate::types::{InlineParameterModifier, Ty, TypeName};

/// What the provider normalized for one selected dependency callable, copied without
/// reinterpretation. None of it is recovered from a spelling.
#[derive(Clone, Debug)]
pub struct BackendCallableFact {
    /// Kotlin declaration spelling and optional callable-reflection spelling published for this
    /// exact identity. Neither is lookup input at this boundary.
    pub name: String,
    pub reflection_name: Option<String>,
    /// Target owner selected by the provider. This is a physical realization fact (a facade or a
    /// mapped platform class), not the declaration's semantic Kotlin owner.
    pub physical_owner: TypeName,
    pub kind: ExternalCallableKind,
    pub owner_is_interface: bool,
    pub compiler_intrinsic: Option<CompilerIntrinsic>,
    pub semantic_role: Option<SemanticCallRole>,
    pub member_realization: MemberRealization,
    pub params: Vec<Ty>,
    pub physical_params: Vec<Ty>,
    pub physical_ret: Ty,
    pub descriptor: String,
    pub inline: InlineKind,
    pub source_receiver: Option<Ty>,
    pub context_count: usize,
    pub declared_params: Option<Box<[Ty]>>,
    pub inline_modifiers: Box<[InlineParameterModifier]>,
    pub default_realization: Option<Box<DefaultCallRealization>>,
    pub nonvirtual_realization: Option<Box<NonvirtualCallRealization>>,
    pub generic_sig: Option<Box<GenericSig>>,
}

/// What the provider normalized for one selected dependency property.
#[derive(Clone, Debug, PartialEq)]
pub struct BackendPropertyFact {
    /// The property's Kotlin name, decoded by its provider.
    pub name: Box<str>,
    pub getter: ExternalCallableId,
    pub setter: Option<ExternalCallableId>,
    /// The declaring owner the provider published for its getter.
    pub owner: TypeName,
    /// The exact result type of the provider's getter declaration, before any use-site
    /// substitution: the property's type as its declaration states it.
    pub result: Ty,
    /// The provider normalized this property as its value class's underlying storage property.
    pub declares_value_class_storage: bool,
    /// Provider-normalized compile-time payload. Common IR owns the semantic value; a target may
    /// choose a physical constant representation but never asks the provider for it again.
    pub compile_time_constant: Option<crate::ir::IrConst>,
}

/// A checked file referenced a dependency identity its provider cannot answer for. Checked IR only
/// holds identities a provider assigned, so this is an internal error, never a user diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyFactError {
    UnknownCallable(ExternalCallableId),
    UnknownProperty(ExternalPropertyId),
    InvalidPropertyConstant(ExternalPropertyId),
}

/// The frozen dependency facts of one checked file. It holds a fact for every dependency callable
/// and property identity the file's IR references, including the accessors of each property.
#[derive(Debug, Default)]
pub struct CheckedBackendCallables {
    callables: HashMap<ExternalCallableId, BackendCallableFact>,
    properties: HashMap<ExternalPropertyId, BackendPropertyFact>,
}

impl CheckedBackendCallables {
    /// Copy the provider's facts for every dependency identity `ir` references.
    pub(crate) fn freeze(
        ir: &crate::ir::IrFile,
        provider: &dyn SymbolSource,
    ) -> Result<Self, DependencyFactError> {
        let referenced = references::referenced_dependencies(ir);
        let mut facts = Self::default();
        let mut callable = |identity| provider.external_callable(identity);
        for property in referenced.properties {
            let realization = provider
                .external_property(property)
                .ok_or(DependencyFactError::UnknownProperty(property))?;
            let compile_time_constant = realization
                .compile_time_constant
                .as_ref()
                .map(|constant| {
                    crate::ir::IrConst::from_library_constant(constant)
                        .ok_or(DependencyFactError::InvalidPropertyConstant(property))
                })
                .transpose()?;
            let owner = facts
                .freeze_callable(realization.getter, &mut callable)?
                .physical_owner;
            let result = provider
                .external_callable(realization.getter)
                .ok_or(DependencyFactError::UnknownCallable(realization.getter))?
                .callable
                .ret;
            if let Some(setter) = realization.setter {
                facts.freeze_callable(setter, &mut callable)?;
            }
            facts.properties.insert(
                property,
                BackendPropertyFact {
                    name: realization.name.into_boxed_str(),
                    getter: realization.getter,
                    setter: realization.setter,
                    owner,
                    result,
                    declares_value_class_storage: realization.declares_value_class_storage,
                    compile_time_constant,
                },
            );
        }
        facts.freeze_callables(referenced.callables, callable)?;
        Ok(facts)
    }

    /// Freeze dependency identities on `super` calls introduced after the initial handoff.
    ///
    /// Target plugins run inside the backend and may append checked super calls only after the
    /// ordinary file facts were copied. Re-scan that narrow carrier in the now-final IR at this one
    /// boundary; identities already present remain the original frozen records, while each newly
    /// selected identity is copied exactly once before super-call realization consumes it.
    pub(crate) fn freeze_plugin_super_callables(
        &mut self,
        ir: &crate::ir::IrFile,
        provider: impl FnMut(
            ExternalCallableId,
        ) -> Option<crate::libraries::ExternalCallableRealization>,
    ) -> Result<(), DependencyFactError> {
        self.freeze_callables(references::external_super_callables(ir), provider)
    }

    fn freeze_callables(
        &mut self,
        identities: impl IntoIterator<Item = ExternalCallableId>,
        mut provider: impl FnMut(
            ExternalCallableId,
        ) -> Option<crate::libraries::ExternalCallableRealization>,
    ) -> Result<(), DependencyFactError> {
        for identity in identities {
            self.freeze_callable(identity, &mut provider)?;
        }
        Ok(())
    }

    fn freeze_callable(
        &mut self,
        identity: ExternalCallableId,
        provider: &mut impl FnMut(
            ExternalCallableId,
        ) -> Option<crate::libraries::ExternalCallableRealization>,
    ) -> Result<&BackendCallableFact, DependencyFactError> {
        let fact = match self.callables.entry(identity) {
            Entry::Occupied(fact) => fact.into_mut(),
            Entry::Vacant(slot) => {
                let realization =
                    provider(identity).ok_or(DependencyFactError::UnknownCallable(identity))?;
                let callable = realization.callable;
                slot.insert(BackendCallableFact {
                    name: callable.name,
                    reflection_name: callable.reflection_name,
                    physical_owner: callable.owner,
                    kind: realization.kind,
                    owner_is_interface: callable.owner_is_interface,
                    compiler_intrinsic: callable.compiler_intrinsic,
                    semantic_role: callable.semantic_role,
                    member_realization: callable.member_realization,
                    params: callable.params,
                    physical_params: callable.physical_params,
                    physical_ret: callable.physical_ret,
                    descriptor: callable.descriptor,
                    inline: callable.inline,
                    source_receiver: callable.source_receiver,
                    context_count: callable.context_count,
                    declared_params: callable.declared_params,
                    inline_modifiers: callable.inline_modifiers,
                    default_realization: callable.default_realization,
                    nonvirtual_realization: callable.nonvirtual_realization,
                    generic_sig: callable.generic_sig,
                })
            }
        };
        Ok(fact)
    }

    /// The fact for a dependency callable identity this file's IR holds. `None` means the identity
    /// is not one of this file's.
    pub fn callable(&self, identity: ExternalCallableId) -> Option<&BackendCallableFact> {
        self.callables.get(&identity)
    }

    /// The fact for a dependency property identity this file's IR holds. `None` means the identity
    /// is not one of this file's.
    pub fn property(&self, identity: ExternalPropertyId) -> Option<&BackendPropertyFact> {
        self.properties.get(&identity)
    }
}
