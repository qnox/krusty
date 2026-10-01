//! Frozen facts about the dependency callables one checked file's IR selected.
//!
//! Checked IR names a dependency callable only by the opaque identity its provider assigned. This
//! table copies, once per file at the frontend/backend boundary, what that provider normalized for
//! exactly the identities the file's IR holds, so a backend can read a dependency callable's facts
//! without asking the provider about it while it emits. It covers callables only; dependency
//! classifiers still reach a backend through `CheckedBackendClassifiers`, which asks the provider.
//! It answers only for an identity the IR already holds: there is no lookup by name, owner, or
//! signature.

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

/// A checked file referenced a dependency identity its provider cannot answer for. Checked IR only
/// holds identities a provider assigned, so this is an internal error, never a user diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DependencyFactError {
    UnknownCallable(ExternalCallableId),
    UnknownProperty(ExternalPropertyId),
}

/// The frozen dependency facts of one checked file. It holds a fact for every dependency callable
/// identity the file's IR references.
#[derive(Debug, Default)]
pub struct CheckedBackendCallables {
    callables: HashMap<ExternalCallableId, BackendCallableFact>,
}

impl CheckedBackendCallables {
    /// Copy the provider's facts for every dependency identity `ir` references.
    pub(crate) fn freeze(
        ir: &crate::ir::IrFile,
        provider: &dyn SymbolSource,
    ) -> Result<Self, DependencyFactError> {
        let referenced = references::referenced_dependencies(ir);
        let mut facts = Self::default();
        for callable in referenced.callables {
            facts.freeze_callable(callable, provider)?;
        }
        for property in referenced.properties {
            let realization = provider
                .external_property(property)
                .ok_or(DependencyFactError::UnknownProperty(property))?;
            facts.freeze_callable(realization.getter, provider)?;
            if let Some(setter) = realization.setter {
                facts.freeze_callable(setter, provider)?;
            }
        }
        Ok(facts)
    }

    fn freeze_callable(
        &mut self,
        identity: ExternalCallableId,
        provider: &dyn SymbolSource,
    ) -> Result<&BackendCallableFact, DependencyFactError> {
        let fact = match self.callables.entry(identity) {
            Entry::Occupied(fact) => fact.into_mut(),
            Entry::Vacant(slot) => {
                let realization = provider
                    .external_callable(identity)
                    .ok_or(DependencyFactError::UnknownCallable(identity))?;
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
}
