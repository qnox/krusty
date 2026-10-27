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

use crate::fx_hash::HashMap;
use std::collections::hash_map::Entry;

use crate::fir::{ExternalCallableId, ExternalPropertyId};
pub use crate::libraries::CompilerIntrinsic as BackendCompilerIntrinsic;
use crate::libraries::{
    DefaultCallRealization, DirectCFunction, ExternalCallableKind, GenericSig, InlineKind,
    KlibBodyCompanionReceiver, KlibDeclarationSignature, KlibDependencyCallable, MemberRealization,
    NonvirtualCallRealization, SingletonDispatch,
};
use crate::symbol_source::SymbolSource;
pub use crate::types::SemanticCallRole as BackendSemanticCallRole;
use crate::types::{InlineParameterModifier, SemanticCallableOwner, Ty, TypeName};

/// What the provider normalized for one selected dependency callable, copied without
/// reinterpretation. None of it is recovered from a spelling.
#[derive(Clone, Debug)]
pub struct BackendCallableFact {
    /// Kotlin declaration spelling, target realization spelling, and optional callable-reflection
    /// spelling published for this exact identity. None is lookup input at this boundary.
    pub name: String,
    pub physical_name: Option<String>,
    pub reflection_name: Option<String>,
    /// Target owner selected by the provider. This is a physical realization fact (a facade or a
    /// mapped platform class), not the declaration's semantic Kotlin owner.
    pub physical_owner: TypeName,
    /// Declaration visibility. A protected member stays callable from a subclass, and a nested
    /// class of that subclass reaches it through an accessor rather than a public invoke.
    pub visibility: crate::types::Visibility,
    /// Exact semantic declaration namespace, independent of the physical realization kind.
    pub declaration_owner: Option<SemanticCallableOwner>,
    /// Exact singleton value checked call selection chose as this callable's implicit dispatch
    /// receiver. This is distinct from `declaration_owner`: a companion member may be named through
    /// its enclosing classifier while dispatching on the companion object. The backend handoff
    /// freezes this from FIR because the provider's base declaration record predates selection.
    pub singleton_dispatch: Option<SingletonDispatch>,
    pub kind: ExternalCallableKind,
    pub owner_is_interface: bool,
    pub compiler_intrinsic: Option<BackendCompilerIntrinsic>,
    pub semantic_role: Option<BackendSemanticCallRole>,
    /// Exact declarations this declaration overrides, as checked FIR froze them from the member
    /// hierarchy. A target keys a specially implemented language declaration on them.
    pub overridden_declarations: Box<[crate::types::OverriddenDeclaration]>,
    pub member_realization: MemberRealization,
    pub params: Vec<Ty>,
    pub physical_params: Vec<Ty>,
    /// Exact identities parallel to the callable's semantic source parameters, published by the
    /// selected declaration's provider. `physical_parameter_plan` joins them to ABI-only slots.
    /// A backend may format the resulting identities but must not invent them from arity.
    pub parameter_identities: Box<[crate::fir::ResolvedParameterIdentity]>,
    /// Exact physical slot roles published by the provider while it aligned metadata with the
    /// classfile declaration. Backends consume this frozen plan; they do not reconstruct roles
    /// from arity or descriptor spelling.
    pub physical_parameter_plan: Option<Box<[crate::libraries::PhysicalParameterSlot]>>,
    /// Exact semantic declaration result before target representation. A call site may expose a
    /// substituted or erased carrier, while intrinsic realization still needs the declaration's
    /// value-class identity.
    pub ret: Ty,
    pub physical_ret: Ty,
    pub descriptor: String,
    pub inline: InlineKind,
    pub source_receiver: Option<Ty>,
    pub context_count: usize,
    pub declared_params: Option<Box<[Ty]>>,
    pub declared_ret: Option<Ty>,
    pub inline_modifiers: Box<[InlineParameterModifier]>,
    pub default_realization: Option<Box<DefaultCallRealization>>,
    pub nonvirtual_realization: Option<Box<NonvirtualCallRealization>>,
    pub overridden_call_realizations: Box<[crate::libraries::OverriddenCallRealization]>,
    pub generic_sig: Option<Box<GenericSig>>,
    pub reified_type_parameter_ordinals: Box<[u32]>,
    /// Exact serialized-IR identity the provider published for this declaration, if any.
    pub declaration_signature: Option<KlibDeclarationSignature>,
    /// Exact KLIB module and declaration identity for target linkage, if this declaration came
    /// from a KLIB provider.
    pub dependency_callable: Option<KlibDependencyCallable>,
    /// Exact Native C entry the selected interop declaration's provider authenticated.
    pub direct_c_function: Option<DirectCFunction>,
    /// Frozen KLIB-only receiver contract for an associated companion extension body.
    pub(crate) klib_body_companion_receiver: Option<KlibBodyCompanionReceiver>,
    pub(crate) klib_body_type_parameters: Box<[Ty]>,
    pub(crate) klib_body_declaration_modifiers:
        Option<crate::libraries::KlibBodyDeclarationModifiers>,
    pub(crate) klib_body_class_contract: Option<crate::libraries::KlibBodyClassContract>,
    pub(crate) klib_body_vararg_index: Option<usize>,
    pub(crate) klib_body_parameter_defaults: Option<crate::libraries::ParamFlags>,
}

impl BackendCallableFact {
    pub fn physical_name(&self) -> &str {
        self.physical_name.as_deref().unwrap_or(&self.name)
    }

    /// Whether this selected callable is a top-level declaration or extension.
    ///
    /// This is copied from the provider's declaration kind at the handoff. Backends use the
    /// normalized shape and never need to depend on the provider-owned kind enum itself.
    pub fn is_top_level(&self) -> bool {
        matches!(
            self.kind,
            ExternalCallableKind::TopLevel | ExternalCallableKind::Extension
        )
    }

    /// Whether this selected callable is a receiver-less top-level or associated declaration.
    ///
    /// The provider's kind remains behind the checked handoff; a backend asks only for the exact
    /// declaration shape it can emit.
    pub fn is_top_level_function(&self) -> bool {
        self.kind == ExternalCallableKind::TopLevel
    }

    /// Whether this selected callable has a source extension receiver parameter.
    pub fn is_extension(&self) -> bool {
        self.kind == ExternalCallableKind::Extension
    }

    /// Whether this exact dependency declaration is an instance member.
    pub fn is_member_function(&self) -> bool {
        self.kind == ExternalCallableKind::Member
    }

    /// Whether this instance-dispatched declaration also has a source extension receiver in its
    /// parameter list. KLIB normalizes this as a member because dispatch ownership is primary;
    /// the receiver side fact distinguishes it from an ordinary member without changing the
    /// provider-wide callable-kind model.
    pub fn is_member_extension(&self) -> bool {
        self.is_member_function() && self.source_receiver.is_some()
    }

    /// The serialized receiver coordinate retained by an associated companion extension body.
    /// Source calls have no receiver operand; a target may insert an inert reference only when
    /// this exact provider contract and the lowered body's receiver effect are both present.
    pub(crate) fn klib_body_companion_receiver_position(&self) -> Option<usize> {
        self.klib_body_companion_receiver
            .as_ref()
            .map(|_| self.context_count)
    }

    /// Authenticate a serialized associated-extension call whose dependency body already carries
    /// the retained companion receiver in `call_params`. This is a provider-owned callable-shape
    /// check, so target registries do not need to import the compiler type model to repeat it.
    pub(crate) fn serialized_companion_receiver_position(
        &self,
        call_params: &[Ty],
        extension_receiver_parameter: Option<u32>,
        has_dispatch_receiver: bool,
        emitted_receiver_ty: Option<Ty>,
    ) -> Option<usize> {
        if has_dispatch_receiver || !self.is_top_level_function() {
            return None;
        }
        let position = usize::try_from(extension_receiver_parameter?).ok()?;
        if self.klib_body_companion_receiver_position() != Some(position)
            || position > self.params.len()
            || call_params.len() != self.params.len() + 1
            || call_params[..position] != self.params[..position]
            || call_params.get(position).copied() != emitted_receiver_ty
            || call_params[position + 1..] != self.params[position..]
        {
            return None;
        }
        Some(position)
    }

    /// Whether `classifier` is the exact singleton declaration owner of this selected member.
    /// This authenticates an already-lowered dependency support call; it does not discover a
    /// callable or infer singleton semantics from a name.
    pub fn is_singleton_member_of(&self, classifier: TypeName) -> bool {
        self.is_member_function()
            && self
                .singleton_dispatch
                .is_some_and(|dispatch| dispatch.classifier == classifier)
    }

    /// Whether one checked call supplies exactly the source/serialized receiver shape of this
    /// dependency declaration's emitted KLIB body.
    ///
    /// Source extension calls carry their receiver separately. Calls lowered from serialized KLIB
    /// bodies already carry it at the provider-recorded context coordinate. Keeping that semantic
    /// comparison on the frozen fact prevents a backend from depending on the compiler type model.
    pub fn matches_dependency_body_call(
        &self,
        call_params: &[Ty],
        extension_receiver_parameter: Option<u32>,
        has_dispatch_receiver: bool,
    ) -> bool {
        match (
            self.is_top_level_function(),
            self.is_extension(),
            extension_receiver_parameter,
            has_dispatch_receiver,
        ) {
            (true, false, None, false) => true,
            (false, true, None, true) => true,
            (false, false, None, true) if self.is_member_function() => true,
            (false, true, Some(position), false) => {
                usize::try_from(position).ok().is_some_and(|position| {
                    position == self.context_count
                        && self.params == call_params
                        && self.source_receiver.is_some()
                        && call_params.get(position) == self.source_receiver.as_ref()
                })
            }
            (false, false, Some(position), true) if self.is_member_extension() => {
                usize::try_from(position).ok().is_some_and(|position| {
                    position == self.context_count
                        && self.params == call_params
                        && call_params.get(position) == self.source_receiver.as_ref()
                })
            }
            _ => false,
        }
    }

    /// The target-free declaration view used when a selected KLIB body is lowered to common IR.
    pub fn klib_body_callable(&self) -> Option<crate::libraries::KlibBodyCallable<'_>> {
        self.klib_body_callable_with_singleton(self.singleton_dispatch)
    }

    fn klib_body_callable_with_singleton(
        &self,
        singleton_dispatch: Option<SingletonDispatch>,
    ) -> Option<crate::libraries::KlibBodyCallable<'_>> {
        crate::libraries::KlibBodyCallable::new(
            crate::libraries::KlibBodyCallableShape {
                name: &self.name,
                kind: self.kind,
                visibility: self.visibility,
                semantic_role: self.semantic_role,
                params: &self.params,
                ret: self.ret,
                parameter_identities: &self.parameter_identities,
                context_count: self.context_count,
                vararg_index: self.klib_body_vararg_index,
                parameter_defaults: self.klib_body_parameter_defaults,
                generic_signature: self.generic_sig.as_deref(),
                type_parameters: &self.klib_body_type_parameters,
                reified_type_parameter_ordinals: &self.reified_type_parameter_ordinals,
                compiler_intrinsic: self
                    .compiler_intrinsic
                    .or(self.member_realization.compiler_intrinsic()),
                range_construction: self.member_realization.range_construction(),
                declaration_modifiers: self.klib_body_declaration_modifiers,
                class_contract: self.klib_body_class_contract.as_ref(),
            },
            singleton_dispatch,
            self.klib_body_companion_receiver.as_ref(),
            self.declaration_owner,
            self.dependency_callable.as_ref(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extension_fact() -> BackendCallableFact {
        let params = vec![Ty::String, Ty::Float, Ty::Int];
        BackendCallableFact {
            name: "sample".to_string(),
            physical_name: None,
            reflection_name: None,
            physical_owner: TypeName::ROOT,
            visibility: crate::types::Visibility::Public,
            declaration_owner: None,
            singleton_dispatch: None,
            kind: ExternalCallableKind::Extension,
            owner_is_interface: false,
            compiler_intrinsic: None,
            semantic_role: None,
            overridden_declarations: Box::new([]),
            member_realization: MemberRealization::Dispatch,
            params: params.clone(),
            physical_params: params,
            parameter_identities: Box::new([]),
            physical_parameter_plan: None,
            ret: Ty::Unit,
            physical_ret: Ty::Unit,
            descriptor: String::new(),
            inline: InlineKind::None,
            source_receiver: Some(Ty::Float),
            context_count: 1,
            declared_params: None,
            declared_ret: None,
            inline_modifiers: Box::new([]),
            default_realization: None,
            nonvirtual_realization: None,
            overridden_call_realizations: Box::new([]),
            generic_sig: None,
            reified_type_parameter_ordinals: Box::new([]),
            declaration_signature: None,
            dependency_callable: None,
            direct_c_function: None,
            klib_body_companion_receiver: None,
            klib_body_type_parameters: Box::new([]),
            klib_body_declaration_modifiers: None,
            klib_body_class_contract: None,
            klib_body_vararg_index: None,
            klib_body_parameter_defaults: None,
        }
    }

    #[test]
    fn dependency_extensions_keep_their_provider_receiver_coordinate() {
        let mut fact = extension_fact();
        let params = fact.params.clone();
        assert!(fact.matches_dependency_body_call(&params, Some(1), false));
        assert!(!fact.matches_dependency_body_call(&params, Some(0), false));
        assert!(!fact.matches_dependency_body_call(&params, Some(1), true));

        fact.kind = ExternalCallableKind::Member;
        assert!(fact.is_member_extension());
        assert!(fact.matches_dependency_body_call(&params, Some(1), true));
        assert!(!fact.matches_dependency_body_call(&params, Some(0), true));
        assert!(!fact.matches_dependency_body_call(&params, Some(1), false));
    }

    fn accessor_facts() -> CheckedBackendCallables {
        let mut facts = CheckedBackendCallables::default();
        let mut getter = extension_fact();
        getter.params.pop();
        getter.ret = Ty::Long;
        facts
            .callables
            .insert(ExternalCallableId::from_raw(1), getter);
        facts
            .callables
            .insert(ExternalCallableId::from_raw(2), extension_fact());
        facts.properties.insert(
            ExternalPropertyId::from_raw(0),
            BackendPropertyFact {
                name: "selected".into(),
                getter: ExternalCallableId::from_raw(1),
                setter: Some(ExternalCallableId::from_raw(2)),
                owner: TypeName::ROOT,
                result: Ty::Long,
                declares_value_class_storage: false,
                compile_time_constant: None,
            },
        );
        facts
    }

    fn property_read(receiver: Option<u32>) -> crate::ir::IrCheckedOperation {
        crate::ir::IrCheckedOperation::ExternalPropertyRead {
            target: ExternalPropertyId::from_raw(0),
            dispatch: crate::ir::IrPropertyDispatch::Ordinary,
            receiver,
            arguments: if receiver.is_some() {
                vec![20]
            } else {
                vec![20, 10]
            },
            parameters: if receiver.is_some() {
                vec![Ty::String]
            } else {
                vec![Ty::String, Ty::Float]
            },
            result: Ty::Long,
            source_receiver: None,
        }
    }

    #[test]
    fn dependency_accessors_retain_exact_targets_and_receiver_coordinates() {
        let facts = accessor_facts();
        for (receiver, expected_position) in [(Some(10), None), (None, Some(1))] {
            let operation = property_read(receiver);
            let access = facts.dependency_accessor(&operation).unwrap();
            assert_eq!(access.target, ExternalCallableId::from_raw(1));
            assert_eq!(access.receiver, receiver);
            let crate::ir::Callee::External {
                target,
                params,
                ret,
                substitutions,
                defaults,
                extension_receiver_parameter,
                default_provider,
            } = access.callee().unwrap()
            else {
                panic!("an exact external getter")
            };
            assert_eq!(target, ExternalCallableId::from_raw(1));
            assert_eq!(
                params,
                if receiver.is_some() {
                    vec![Ty::String]
                } else {
                    vec![Ty::String, Ty::Float]
                }
            );
            assert_eq!(ret, Ty::Long);
            assert!(substitutions.is_empty());
            assert!(defaults.is_empty());
            assert_eq!(default_provider, None);
            assert_eq!(extension_receiver_parameter, expected_position);
        }
        let operation = crate::ir::IrCheckedOperation::ExternalPropertyWrite {
            target: ExternalPropertyId::from_raw(0),
            dispatch: crate::ir::IrPropertyDispatch::Ordinary,
            receiver: Some(10),
            arguments: vec![20, 30],
            parameters: vec![Ty::String, Ty::Int],
            result: Ty::Unit,
            source_receiver: None,
        };
        let access = facts.dependency_accessor(&operation).unwrap();
        assert_eq!(access.target, ExternalCallableId::from_raw(2));
        assert_eq!(access.arguments, [20, 30]);
        assert!(
            matches!(access.callee().unwrap(), crate::ir::Callee::External {
            target, ret: Ty::Unit, extension_receiver_parameter: None, ..
        } if target == ExternalCallableId::from_raw(2))
        );
    }

    #[test]
    fn dependency_accessors_do_not_invent_setters_or_generic_substitutions() {
        let mut facts = accessor_facts();
        let mut operation = property_read(Some(10));
        if let crate::ir::IrCheckedOperation::ExternalPropertyRead { dispatch, .. } = &mut operation
        {
            *dispatch = crate::ir::IrPropertyDispatch::Super {
                owner: TypeName::ROOT,
                interface: false,
            };
        }
        assert_eq!(
            facts
                .dependency_accessor(&operation)
                .unwrap()
                .callee()
                .unwrap_err(),
            "a dependency accessor export called through super"
        );
        facts
            .callables
            .get_mut(&ExternalCallableId::from_raw(1))
            .unwrap()
            .generic_sig = Some(Box::new(GenericSig {
            formals: vec!["owned-T".to_owned()],
            formal_bounds: vec![Vec::new()],
            receiver: None,
            params: Vec::new(),
            ret: Ty::Long,
            return_policy: Default::default(),
        }));
        let operation = property_read(Some(10));
        assert_eq!(
            facts
                .dependency_accessor(&operation)
                .unwrap()
                .callee()
                .unwrap_err(),
            "a dependency property without checked generic substitutions"
        );
        facts
            .properties
            .get_mut(&ExternalPropertyId::from_raw(0))
            .unwrap()
            .setter = None;
        let operation = crate::ir::IrCheckedOperation::ExternalPropertyWrite {
            target: ExternalPropertyId::from_raw(0),
            dispatch: crate::ir::IrPropertyDispatch::Ordinary,
            receiver: Some(10),
            arguments: vec![20, 30],
            parameters: vec![Ty::String, Ty::Int],
            result: Ty::Unit,
            source_receiver: None,
        };
        assert!(facts.dependency_accessor(&operation).is_none());
    }

    #[test]
    fn a_member_shape_requires_its_exact_provider_selected_singleton() {
        let owner = crate::types::type_name("kotlin/Float$Companion");
        let mut fact = BackendCallableFact {
            name: "fromBits".to_string(),
            physical_name: None,
            reflection_name: None,
            physical_owner: owner,
            visibility: crate::types::Visibility::Public,
            declaration_owner: Some(SemanticCallableOwner::Classifier(crate::types::type_name(
                "kotlin/Float",
            ))),
            singleton_dispatch: Some(SingletonDispatch { classifier: owner }),
            kind: ExternalCallableKind::Member,
            owner_is_interface: false,
            compiler_intrinsic: None,
            semantic_role: None,
            overridden_declarations: Box::new([]),
            member_realization: MemberRealization::Dispatch,
            params: vec![Ty::Int],
            physical_params: vec![Ty::Int],
            parameter_identities: Box::new([]),
            physical_parameter_plan: None,
            ret: Ty::Float,
            physical_ret: Ty::Float,
            descriptor: String::new(),
            inline: InlineKind::None,
            source_receiver: None,
            context_count: 0,
            declared_params: None,
            declared_ret: None,
            inline_modifiers: Box::new([]),
            default_realization: None,
            nonvirtual_realization: None,
            overridden_call_realizations: Box::new([]),
            generic_sig: None,
            reified_type_parameter_ordinals: Box::new([]),
            declaration_signature: None,
            dependency_callable: None,
            direct_c_function: None,
            klib_body_companion_receiver: None,
            klib_body_type_parameters: Box::new([]),
            klib_body_declaration_modifiers: None,
            klib_body_class_contract: None,
            klib_body_vararg_index: None,
            klib_body_parameter_defaults: None,
        };
        assert!(fact.matches_dependency_body_call(&[Ty::Int], None, true));
        assert!(!fact.matches_dependency_body_call(&[Ty::Int], None, false));
        assert!(fact.is_singleton_member_of(owner));
        fact.singleton_dispatch = Some(SingletonDispatch {
            classifier: crate::types::type_name("kotlin/Double$Companion"),
        });
        assert!(!fact.is_singleton_member_of(owner));
    }
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
    InvalidSingletonDispatch(crate::ir::ExprId),
}

/// The frozen dependency facts of one checked file. It holds a fact for every dependency callable
/// and property identity the file's IR references, including the accessors of each property.
#[derive(Debug, Default)]
pub struct CheckedBackendCallables {
    callables: HashMap<ExternalCallableId, BackendCallableFact>,
    properties: HashMap<ExternalPropertyId, BackendPropertyFact>,
    singleton_dispatches: HashMap<crate::ir::ExprId, SingletonDispatch>,
    singleton_selections: HashMap<ExternalCallableId, Vec<SingletonDispatch>>,
}

impl CheckedBackendCallables {
    /// The exact declaration view for one emitted edge, retaining only its checked singleton.
    pub(crate) fn klib_body_callable_at(
        &self,
        expression: crate::ir::ExprId,
        target: ExternalCallableId,
    ) -> Option<crate::libraries::KlibBodyCallable<'_>> {
        self.callable(target)?
            .klib_body_callable_with_singleton(self.singleton_dispatch(expression))
    }

    /// Selected KLIB callables in provider-identity order. Dependency-body lowering consumes this
    /// frozen semantic view; deterministic ordering keeps generated support-unit symbols stable.
    pub(crate) fn klib_body_callables(&self) -> Vec<crate::libraries::KlibBodyCallable<'_>> {
        let mut callables = self.callables.iter().collect::<Vec<_>>();
        callables.sort_unstable_by_key(|(identity, _)| identity.raw());
        let mut selected = Vec::new();
        for (identity, callable) in callables {
            match self.singleton_selections.get(identity) {
                Some(singletons) => selected.extend(singletons.iter().filter_map(|singleton| {
                    callable.klib_body_callable_with_singleton(Some(*singleton))
                })),
                None => selected.extend(callable.klib_body_callable()),
            }
        }
        selected
    }

    /// Exact singleton dispatch chosen for this checked call edge, if it has one.
    pub(crate) fn singleton_dispatch(
        &self,
        expression: crate::ir::ExprId,
    ) -> Option<SingletonDispatch> {
        self.singleton_dispatches.get(&expression).copied()
    }

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
            let getter = provider
                .external_callable(realization.getter)
                .ok_or(DependencyFactError::UnknownCallable(realization.getter))?
                .callable;
            let result = getter
                .generic_sig
                .as_ref()
                .map(|signature| signature.ret)
                .or(getter.declared_ret)
                .unwrap_or(getter.ret);
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
        let mut singleton_dispatches = ir
            .external_singleton_dispatches
            .iter()
            .map(|(&expression, &classifier)| (expression, classifier))
            .collect::<Vec<_>>();
        singleton_dispatches.sort_unstable_by_key(|(expression, _)| *expression);
        for (expression, classifier) in singleton_dispatches {
            let crate::ir::IrExpr::Call { callee, .. } = ir.expr(expression) else {
                return Err(DependencyFactError::InvalidSingletonDispatch(expression));
            };
            let crate::ir::Callee::External { target, .. } = &**callee else {
                return Err(DependencyFactError::InvalidSingletonDispatch(expression));
            };
            if !facts.callables.contains_key(target) {
                return Err(DependencyFactError::UnknownCallable(*target));
            }
            let dispatch = SingletonDispatch { classifier };
            facts.singleton_dispatches.insert(expression, dispatch);
            let selections = facts.singleton_selections.entry(*target).or_default();
            if !selections.contains(&dispatch) {
                selections.push(dispatch);
            }
        }
        for (identity, overridden) in &ir.external_overridden_declarations {
            if let Some(fact) = facts.callables.get_mut(identity) {
                fact.overridden_declarations = overridden.clone();
            }
        }
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
                let parameter_identities = realization.parameter_identities;
                let declaration_signature = realization.declaration_signature;
                let dependency_callable = realization.dependency_callable;
                let direct_c_function = realization.direct_c_function;
                let klib_body_companion_receiver = realization.klib_body_companion_receiver;
                let klib_body_type_parameters = realization.klib_body_type_parameters;
                let klib_body_declaration_modifiers = realization.klib_body_declaration_modifiers;
                let klib_body_class_contract = realization.klib_body_class_contract;
                let klib_body_vararg_index = realization.klib_body_vararg_index;
                let klib_body_parameter_defaults = realization.klib_body_parameter_defaults;
                let callable = realization.callable;
                slot.insert(BackendCallableFact {
                    name: callable.name,
                    physical_name: callable.physical_name,
                    reflection_name: callable.reflection_name,
                    physical_owner: callable.owner,
                    visibility: callable.visibility,
                    declaration_owner: realization.declaration_owner,
                    singleton_dispatch: None,
                    kind: realization.kind,
                    owner_is_interface: callable.owner_is_interface,
                    compiler_intrinsic: callable.compiler_intrinsic,
                    semantic_role: callable.semantic_role,
                    overridden_declarations: Box::new([]),
                    member_realization: callable.member_realization,
                    params: callable.params,
                    physical_params: callable.physical_params,
                    parameter_identities,
                    physical_parameter_plan: callable.physical_parameter_plan,
                    ret: callable.ret,
                    physical_ret: callable.physical_ret,
                    descriptor: callable.descriptor,
                    inline: callable.inline,
                    source_receiver: callable.source_receiver,
                    context_count: callable.context_count,
                    declared_params: callable.declared_params,
                    declared_ret: callable.declared_ret,
                    inline_modifiers: callable.inline_modifiers,
                    default_realization: callable.default_realization,
                    nonvirtual_realization: callable.nonvirtual_realization,
                    overridden_call_realizations: callable.overridden_call_realizations,
                    generic_sig: callable.generic_sig,
                    reified_type_parameter_ordinals: callable.reified_type_parameter_ordinals,
                    declaration_signature,
                    dependency_callable,
                    direct_c_function,
                    klib_body_companion_receiver,
                    klib_body_type_parameters,
                    klib_body_declaration_modifiers,
                    klib_body_class_contract,
                    klib_body_vararg_index,
                    klib_body_parameter_defaults,
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
