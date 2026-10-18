//! Stable dependency declaration identities and their provider-owned JVM realizations.
//!
//! Classpath queries can discover a target declaration through a partial spelling index before its
//! complete classifier metadata is materialized. This registry interns the joined physical
//! realization and semantic declaration owner, then monotonically enriches that record; consumers
//! never repeat lookup by spelling.

use super::{
    Classpath, ExternalCallableKind, ExternalCallableRealization, ExternalPropertyRealization,
};
use crate::libraries::LibraryCallable;
use crate::types::{SemanticCallableOwner, Ty, TypeName};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct ExternalPropertyKey {
    getter: crate::fir::ExternalCallableId,
    setter: Option<crate::fir::ExternalCallableId>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct ExternalCallableKey {
    owner: TypeName,
    name: String,
    descriptor: String,
    physical_params: Vec<Ty>,
    physical_ret: Ty,
    default_call: bool,
    kind: ExternalCallableKind,
    declaration_owner: Option<SemanticCallableOwner>,
}

impl Classpath {
    /// Scope a decoded callable signature's own formals to its selected dependency declaration.
    ///
    /// Both Java `Signature` attributes and Kotlin metadata are source-spelled while decoding. The
    /// external callable registry is their first shared stable declaration boundary, so identity
    /// assignment belongs here rather than in resolution. The stored realization and the provider
    /// candidate receive the same normalized contract atomically.
    pub(crate) fn normalize_external_callable_generic_signature(
        &self,
        identity: crate::fir::ExternalCallableId,
        callable: &mut crate::libraries::LibraryCallable,
    ) {
        self.normalize_external_callable_generic_signature_with(
            identity,
            callable,
            |ordinal, source| {
                crate::types::dependency_callable_type_parameter(
                    self.id,
                    identity.raw(),
                    ordinal,
                    source,
                )
            },
        );
    }

    fn normalize_external_callable_generic_signature_with(
        &self,
        identity: crate::fir::ExternalCallableId,
        callable: &mut crate::libraries::LibraryCallable,
        mut formal_identity: impl FnMut(usize, &str) -> &'static str,
    ) {
        callable.external_identity = Some(identity);
        let Some(mut signature) = callable.generic_sig.take() else {
            return;
        };
        let prior_signature = signature.clone();
        signature.assign_declaration_formal_identities(&mut formal_identity);
        let renames = signature.declaration_formal_renames_from(&prior_signature);
        self.normalize_external_callable_types_with(identity, callable, &renames);
        callable.generic_sig = Some(signature);
        if let Some(stored) = self
            .external_callables
            .borrow_mut()
            .get_mut(identity.raw() as usize)
        {
            stored.callable.generic_sig = callable.generic_sig.clone();
        }
    }

    /// Move every semantic type facet carried beside a generic signature to that signature's
    /// declaration identities. Physical descriptors and parameter types deliberately stay intact.
    fn normalize_external_callable_types_with(
        &self,
        identity: crate::fir::ExternalCallableId,
        callable: &mut crate::libraries::LibraryCallable,
        renames: &crate::fx_hash::HashMap<&str, &'static str>,
    ) {
        for parameter in &mut callable.params {
            *parameter = crate::types::ty_rename_params(*parameter, renames);
        }
        callable.ret = crate::types::ty_rename_params(callable.ret, renames);
        callable.vararg_elem = callable
            .vararg_elem
            .map(|element| crate::types::ty_rename_params(element, renames));
        callable.source_receiver = callable
            .source_receiver
            .map(|receiver| crate::types::ty_rename_params(receiver, renames));
        callable.declared_params = callable.declared_params.take().map(|parameters| {
            parameters
                .iter()
                .copied()
                .map(|parameter| crate::types::ty_rename_params(parameter, renames))
                .collect()
        });
        callable.declared_ret = callable
            .declared_ret
            .map(|result| crate::types::ty_rename_params(result, renames));
        callable.overridden_results = callable
            .overridden_results
            .iter()
            .copied()
            .map(|result| crate::types::ty_rename_params(result, renames))
            .collect();
        callable.equality_bound = callable
            .equality_bound
            .map(|bound| crate::types::ty_rename_params(bound, renames));
        callable.contract = callable.contract.as_ref().map(|contract| {
            crate::libraries::normalize_contract_type_parameters(contract, renames)
        });
        if let Some((_, declared)) = &mut callable.enhanced_result.declared {
            *declared = crate::types::ty_rename_params(*declared, renames);
        }
        if let Some(stored) = self
            .external_callables
            .borrow_mut()
            .get_mut(identity.raw() as usize)
        {
            stored.callable.params = callable.params.clone();
            stored.callable.ret = callable.ret;
            stored.callable.vararg_elem = callable.vararg_elem;
            stored.callable.source_receiver = callable.source_receiver;
            stored.callable.declared_params = callable.declared_params.clone();
            stored.callable.declared_ret = callable.declared_ret;
            stored.callable.overridden_results = callable.overridden_results.clone();
            stored.callable.equality_bound = callable.equality_bound;
            stored.callable.contract = callable.contract.clone();
            stored.callable.enhanced_result = callable.enhanced_result.clone();
        }
    }

    /// Re-home every generic facet of one property and both of its accessor realizations to the
    /// property's stable semantic identity. Accessor IDs remain distinct physical targets; they do
    /// not own the source property's formals.
    pub(crate) fn normalize_external_property_generic_signature(
        &self,
        identity: crate::fir::ExternalPropertyId,
        property: &mut crate::libraries::PropertyInfo,
    ) {
        if property.formals.is_empty() {
            return;
        }
        let prior_formals = property.formals.clone();
        let identities = prior_formals
            .iter()
            .enumerate()
            .map(|(ordinal, source)| {
                crate::types::dependency_property_type_parameter(
                    self.id,
                    identity.raw(),
                    ordinal,
                    source,
                )
            })
            .collect::<Vec<_>>();
        let renames = prior_formals
            .iter()
            .zip(&identities)
            .map(|(prior, identity)| (crate::types::intern(prior), *identity))
            .collect::<crate::fx_hash::HashMap<_, _>>();
        property.receiver = property
            .receiver
            .map(|receiver| crate::types::ty_rename_params(receiver, &renames));
        property.ty = crate::types::ty_rename_params(property.ty, &renames);
        property.formals = identities
            .iter()
            .map(|identity| (*identity).to_owned())
            .collect();

        let normalize_accessor = |callable: &mut crate::libraries::LibraryCallable,
                                  this: &Classpath| {
            let external = callable
                .external_identity
                .expect("a registered generic property accessor has an external identity");
            if let Some(signature) = callable.generic_sig.as_ref() {
                assert_eq!(
                    signature.formals.len(),
                    identities.len(),
                    "a property accessor retains every property-owned formal"
                );
                this.normalize_external_callable_generic_signature_with(
                    external,
                    callable,
                    |ordinal, _| identities[ordinal],
                );
            } else {
                // A setter is a physical realization of the property declaration, not a second
                // source callable. Property metadata owns its generic signature; normalize the
                // setter's semantic parameter facets through that exact declaration map without
                // inventing a setter-owned signature.
                this.normalize_external_callable_types_with(external, callable, &renames);
            }
        };
        normalize_accessor(&mut property.getter, self);
        if let Some(setter) = &mut property.setter {
            normalize_accessor(setter, self);
        }
    }

    /// Intern one provider-normalized callable and return the stable identity passed through FIR.
    /// The key joins the exact physical realization with the semantic declaration owner; neither
    /// component is a source lookup key. One JVM method can realize distinct Kotlin declarations.
    pub(crate) fn intern_external_callable(
        &self,
        callable: &LibraryCallable,
        kind: ExternalCallableKind,
        declaration_owner: Option<SemanticCallableOwner>,
    ) -> crate::fir::ExternalCallableId {
        let key = ExternalCallableKey {
            owner: callable.owner,
            name: callable.physical_name().to_string(),
            descriptor: callable.descriptor.clone(),
            physical_params: callable.physical_params.clone(),
            physical_ret: callable.physical_ret,
            default_call: callable.default_call,
            kind,
            declaration_owner,
        };
        if let Some(identity) = self.external_callable_ids.borrow().get(&key).copied() {
            // The same physical declaration can reach this boundary first through a partial
            // spelling-indexed view and later through its complete classifier declaration. Keep
            // the stable identity, but merge the later declaration facets before returning it.
            self.enrich_external_callable(identity, callable);
            return identity;
        }
        let mut callables = self.external_callables.borrow_mut();
        let identity = crate::fir::ExternalCallableId::from_raw(
            u32::try_from(callables.len())
                .expect("too many external callable declarations for packed FIR identity"),
        );
        let mut stored = callable.clone();
        stored.external_identity = Some(identity);
        callables.push(ExternalCallableRealization {
            callable: stored,
            kind,
            declaration_owner,
            parameter_identities: Box::new([]),
            declaration_signature: None,
            dependency_callable: None,
            direct_c_function: None,
            klib_body_companion_receiver: None,
        });
        self.external_callable_ids
            .borrow_mut()
            .insert(key, identity);
        identity
    }

    pub(crate) fn external_callable(
        &self,
        identity: crate::fir::ExternalCallableId,
    ) -> Option<ExternalCallableRealization> {
        self.external_callables
            .borrow()
            .get(identity.raw() as usize)
            .cloned()
    }

    /// Intern the semantic property declaration represented by a provider's accessor pair. The
    /// pair remains JVM-owned; callers outside this module receive only the opaque property id.
    pub(crate) fn intern_external_property(
        &self,
        name: &str,
        getter: crate::fir::ExternalCallableId,
        setter: Option<crate::fir::ExternalCallableId>,
        compile_time_constant: Option<crate::libraries::LibraryConst>,
    ) -> crate::fir::ExternalPropertyId {
        let key = ExternalPropertyKey { getter, setter };
        if let Some(identity) = self.external_property_ids.borrow().get(&key).copied() {
            return identity;
        }
        let declares_value_class_storage = self.getter_declares_value_class_storage(getter);
        let mut properties = self.external_properties.borrow_mut();
        let identity = crate::fir::ExternalPropertyId::from_raw(
            u32::try_from(properties.len())
                .expect("too many external property declarations for packed FIR identity"),
        );
        properties.push(ExternalPropertyRealization {
            name: name.to_string(),
            getter,
            setter,
            declares_value_class_storage,
            compile_time_constant,
        });
        self.external_property_ids
            .borrow_mut()
            .insert(key, identity);
        identity
    }

    pub(crate) fn external_property(
        &self,
        identity: crate::fir::ExternalPropertyId,
    ) -> Option<ExternalPropertyRealization> {
        self.external_properties
            .borrow()
            .get(identity.raw() as usize)
            .cloned()
    }

    /// Publish declaration facets discovered after an exact external identity was first interned.
    /// Classifier construction and spelling-indexed callable construction are intentionally lazy and
    /// can encounter the same physical method in either order. The identity must therefore converge
    /// on the complete provider realization instead of permanently retaining the first partial view.
    pub(crate) fn enrich_external_callable(
        &self,
        identity: crate::fir::ExternalCallableId,
        callable: &LibraryCallable,
    ) {
        let mut callables = self.external_callables.borrow_mut();
        let Some(stored) = callables.get_mut(identity.raw() as usize) else {
            return;
        };
        if stored.callable.default_realization.is_none() {
            stored.callable.default_realization = callable.default_realization.clone();
        }
        if stored.callable.nonvirtual_realization.is_none() {
            stored.callable.nonvirtual_realization = callable.nonvirtual_realization.clone();
        }
        if callable
            .overridden_call_realizations
            .iter()
            .any(|candidate| {
                !stored
                    .callable
                    .overridden_call_realizations
                    .contains(candidate)
            })
        {
            let mut candidates = stored.callable.overridden_call_realizations.to_vec();
            let additions = callable
                .overridden_call_realizations
                .iter()
                .filter(|candidate| !candidates.contains(candidate))
                .cloned()
                .collect::<Vec<_>>();
            candidates.extend(additions);
            stored.callable.overridden_call_realizations = candidates.into_boxed_slice();
        }
        if !stored.callable.inline.can_inline() && callable.inline.can_inline() {
            stored.callable.inline = callable.inline;
        }
        if stored.callable.inline_body_plan.is_none() {
            stored.callable.inline_body_plan = callable.inline_body_plan.clone();
        }
        if stored.callable.declared_ret.is_none() {
            stored.callable.declared_ret = callable.declared_ret;
        }
        if stored.callable.generic_sig.is_none() {
            stored.callable.generic_sig = callable.generic_sig.clone();
        }
        if stored.callable.declared_params.is_none() {
            stored.callable.declared_params = callable.declared_params.clone();
        }
        if stored.callable.inline_modifiers.is_empty() && !callable.inline_modifiers.is_empty() {
            stored.callable.inline_modifiers = callable.inline_modifiers.clone();
        }
        if stored.callable.compiler_intrinsic.is_none() {
            stored.callable.compiler_intrinsic = callable.compiler_intrinsic;
        }
        if stored.callable.semantic_role.is_none() {
            stored.callable.semantic_role = callable.semantic_role;
        }
        if stored.callable.member_realization == crate::libraries::MemberRealization::Dispatch
            && callable.member_realization != crate::libraries::MemberRealization::Dispatch
        {
            stored.callable.member_realization = callable.member_realization;
        }
        // Spelling-indexed construction can intern the physical method before classifier
        // publication copies the declaration's visibility. Public is the incomplete default;
        // a later protected, private, or package-private view of the same method is the
        // declaration.
        if stored.callable.visibility == crate::types::Visibility::Public
            && callable.visibility != crate::types::Visibility::Public
        {
            stored.callable.visibility = callable.visibility;
        }
    }

    /// Attach the declaration's exact physical parameter identities after callable selection has
    /// normalized context and extension-receiver slots. The physical callable identity may be
    /// interned before its complete source facet is decoded, so this enriches that same identity;
    /// it never searches by spelling.
    pub(crate) fn publish_external_callable_parameter_identities(
        &self,
        identity: crate::fir::ExternalCallableId,
        identities: Box<[crate::fir::ResolvedParameterIdentity]>,
    ) {
        let mut callables = self.external_callables.borrow_mut();
        let stored = callables
            .get_mut(identity.raw() as usize)
            .expect("parameter identities name an interned external callable");
        if stored.parameter_identities.is_empty() {
            stored.parameter_identities = identities;
        } else {
            assert_eq!(
                stored.parameter_identities, identities,
                "one external callable identity cannot publish conflicting parameter identities"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    fn generic_callable(owner: TypeName, name: &str, descriptor: &str) -> LibraryCallable {
        let any = Ty::obj("kotlin/Any");
        let parameter = Ty::ty_param("T", any);
        let mut callable =
            LibraryCallable::library(owner, name, vec![parameter], parameter, any, descriptor);
        callable.generic_sig = Some(Box::new(crate::libraries::GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![Vec::new()],
            receiver: None,
            params: vec![parameter],
            ret: parameter,
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        }));
        callable
    }

    #[test]
    fn reinterning_a_physical_callable_preserves_later_declaration_facets() {
        let cp = Classpath::new(vec![]);
        let owner = type_name("review/PhysicalOwner");
        let mut first = LibraryCallable::library(
            owner,
            "physicalOperation",
            vec![],
            Ty::obj("review/Answer"),
            Ty::obj("review/Answer"),
            "()Lreview/Answer;",
        );
        first.physical_name = Some("physicalOperation".to_string());
        first.overridden_call_realizations = vec![crate::libraries::OverriddenCallRealization {
            kind: crate::libraries::OverriddenCallKind::Function,
            declaration_owner: type_name("review/FunctionDeclaration"),
            physical_name: "mappedOperation".to_string(),
            descriptor: "()Ljava/lang/Object;".to_string(),
        }]
        .into_boxed_slice();
        let identity = cp.intern_external_callable(&first, ExternalCallableKind::Member, None);

        let mut enriched = first.clone();
        enriched.semantic_role = Some(crate::types::SemanticCallRole::KotlinFunctionInvoke);
        enriched.overridden_call_realizations = vec![crate::libraries::OverriddenCallRealization {
            kind: crate::libraries::OverriddenCallKind::PropertyGetter,
            declaration_owner: type_name("review/PropertyDeclaration"),
            physical_name: "mappedProperty".to_string(),
            descriptor: "()Ljava/lang/Object;".to_string(),
        }]
        .into_boxed_slice();

        assert_eq!(
            cp.intern_external_callable(&enriched, ExternalCallableKind::Member, None),
            identity
        );
        assert_eq!(
            cp.external_callable(identity)
                .expect("the stable callable")
                .callable
                .overridden_call_realizations,
            vec![
                first.overridden_call_realizations[0].clone(),
                enriched.overridden_call_realizations[0].clone(),
            ]
            .into_boxed_slice()
        );
        assert_eq!(
            cp.external_callable(identity)
                .expect("the enriched callable")
                .callable
                .semantic_role,
            enriched.semantic_role
        );
    }

    /// A member reached first through a partial view has no generic signature yet. Its declaration
    /// supplies one later, and the backend names a reified argument by that signature's formals.
    #[test]
    fn reinterning_a_physical_callable_takes_the_later_generic_signature() {
        let cp = Classpath::new(vec![]);
        let partial = LibraryCallable::library(
            type_name("review/Holder"),
            "name",
            vec![],
            Ty::String,
            Ty::String,
            "()Ljava/lang/String;",
        );
        let identity = cp.intern_external_callable(&partial, ExternalCallableKind::Member, None);

        let mut declared = partial.clone();
        let signature = crate::libraries::GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![Vec::new()],
            receiver: None,
            params: Vec::new(),
            ret: Ty::String,
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        };
        declared.generic_sig = Some(Box::new(signature.clone()));

        assert_eq!(
            cp.intern_external_callable(&declared, ExternalCallableKind::Member, None),
            identity
        );
        assert_eq!(
            cp.external_callable(identity)
                .expect("the enriched callable")
                .callable
                .generic_sig
                .as_deref(),
            Some(&signature)
        );
    }

    #[test]
    fn declaration_owner_is_independent_of_the_physical_realization_kind() {
        let cp = Classpath::new(vec![]);
        let callable = LibraryCallable::library(
            type_name("fixture/physical/FacadeKt"),
            "operation",
            vec![],
            Ty::Unit,
            Ty::Unit,
            "()V",
        );
        let owner = SemanticCallableOwner::Package(type_name("fixture/semantic"));

        // A package property or mapped builtin can reuse a storage/member realization. The
        // declaration namespace remains semantic and must not be inferred from that physical kind.
        let identity = cp.intern_external_callable(
            &callable,
            ExternalCallableKind::StaticFieldRead,
            Some(owner),
        );

        assert_eq!(
            cp.external_callable(identity).unwrap().declaration_owner,
            Some(owner)
        );
    }

    #[test]
    fn same_spelled_generic_constructors_receive_distinct_declaration_identities() {
        let cp = Classpath::new(vec![]);
        let constructor = |owner: TypeName| {
            let any = Ty::obj("kotlin/Any");
            let parameter = Ty::ty_param("T", any);
            let mut member = crate::libraries::LibraryMember::new(
                "<init>".to_owned(),
                vec![parameter],
                Ty::Unit,
                "(Ljava/lang/Object;)V".to_owned(),
            );
            member.owner = Some(owner);
            member.generic_sig = Some(crate::libraries::GenericSig {
                formals: vec!["T".to_owned()],
                formal_bounds: vec![Vec::new()],
                receiver: None,
                params: vec![parameter],
                ret: Ty::obj_args_name(owner, &[parameter]),
                return_policy: crate::libraries::GenericReturnPolicy::Exact,
            });
            let mut callable = LibraryCallable::constructor(owner, &member);
            callable.generic_sig = member.generic_sig.clone().map(Box::new);
            callable.declared_params = member
                .generic_sig
                .as_ref()
                .map(|signature| signature.parameters_with_receiver(0));
            callable
        };
        let mut left = constructor(type_name("fixture/Left"));
        let mut right = constructor(type_name("fixture/Right"));
        let left_id = cp.intern_external_callable(
            &left,
            ExternalCallableKind::Constructor,
            Some(SemanticCallableOwner::Classifier(left.owner)),
        );
        let right_id = cp.intern_external_callable(
            &right,
            ExternalCallableKind::Constructor,
            Some(SemanticCallableOwner::Classifier(right.owner)),
        );
        cp.normalize_external_callable_generic_signature(left_id, &mut left);
        cp.normalize_external_callable_generic_signature(right_id, &mut right);

        let left_formal = &left.generic_sig.as_ref().unwrap().formals[0];
        let right_formal = &right.generic_sig.as_ref().unwrap().formals[0];
        assert_ne!(left_formal, right_formal);
        assert_eq!(crate::types::type_parameter_source_name(left_formal), "T");
        assert_eq!(
            left.ret,
            Ty::Unit,
            "a JVM constructor physically returns Unit"
        );
        assert_eq!(
            cp.external_callable(left_id).unwrap().callable.ret,
            Ty::Unit,
            "the stored realization retains the physical constructor result"
        );
        assert!(crate::types::ty_mentions_param(
            left.params[0],
            &[left_formal.clone()]
        ));
        assert!(crate::types::ty_mentions_param(
            left.generic_sig.as_ref().unwrap().ret,
            &[left_formal.clone()]
        ));
        assert!(crate::types::ty_mentions_param(
            right.params[0],
            &[right_formal.clone()]
        ));
        assert!(crate::types::ty_mentions_param(
            right.generic_sig.as_ref().unwrap().ret,
            &[right_formal.clone()]
        ));
    }

    #[test]
    fn one_generic_property_identity_is_shared_by_both_accessor_realizations() {
        let cp = Classpath::new(vec![]);
        let owner = type_name("fixture/Properties");
        let any = Ty::obj("kotlin/Any");
        let parameter = Ty::ty_param("T", any);
        let receiver = Ty::obj_args("kotlin/collections/List", &[parameter]);
        let signature = crate::libraries::GenericSig {
            formals: vec!["T".to_owned()],
            formal_bounds: vec![Vec::new()],
            receiver: Some(receiver),
            params: Vec::new(),
            ret: parameter,
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        };
        let mut getter = LibraryCallable::library(
            owner,
            "getValue",
            vec![receiver],
            parameter,
            any,
            "(Ljava/lang/Object;)Ljava/lang/Object;",
        );
        getter.source_receiver = Some(receiver);
        getter.generic_sig = Some(Box::new(signature));
        let mut setter = LibraryCallable::library(
            owner,
            "setValue",
            vec![receiver, parameter],
            Ty::Unit,
            Ty::Unit,
            "(Ljava/lang/Object;Ljava/lang/Object;)V",
        );
        setter.source_receiver = Some(receiver);
        let getter_id = cp.intern_external_callable(
            &getter,
            ExternalCallableKind::Member,
            Some(SemanticCallableOwner::Classifier(owner)),
        );
        let setter_id = cp.intern_external_callable(
            &setter,
            ExternalCallableKind::Member,
            Some(SemanticCallableOwner::Classifier(owner)),
        );
        cp.normalize_external_callable_generic_signature(getter_id, &mut getter);
        let property = cp.intern_external_property("value", getter_id, Some(setter_id), None);
        let mut declaration = crate::libraries::PropertyInfo {
            name: "value".to_owned(),
            kind: crate::libraries::PropKind::Extension,
            receiver: Some(receiver),
            associated_classifier: None,
            associated_access_owner: None,
            companion_associated: false,
            formals: vec!["T".to_owned()],
            ty: parameter,
            context_count: 0,
            context_param_names: Vec::new(),
            context_parameter_identities: Vec::new(),
            getter,
            setter: Some(setter),
            setter_visibility: crate::types::Visibility::Public,
            setter_parameter_name: None,
            is_const: false,
            implicit_integer_coercion: false,
            compile_time_constant: None,
            metadata_constant_read: false,
            deprecated_hidden: false,
            accessor_availability: Default::default(),
            is_final: true,
            visibility: crate::types::Visibility::Public,
            owner,
            receiver_rank: 0,
            source_key: None,
            stable_declaration: None,
            source_visible: true,
            getter_declaration: None,
            setter_declaration: None,
            source_member: None,
            producer: crate::libraries::PropertyProducer::KotlinAccessor,
            read_stability: crate::libraries::PropertyReadStability::Unstable,
            return_value_status: None,
        };
        cp.normalize_external_property_generic_signature(property, &mut declaration);

        let property_formal = &declaration.formals[0];
        let getter_formal = &declaration.getter.generic_sig.as_ref().unwrap().formals[0];
        assert_eq!(getter_formal, property_formal);
        assert_eq!(crate::types::type_parameter_source_name(getter_formal), "T");
        assert!(crate::types::ty_mentions_param(
            declaration.getter.params[0],
            &[getter_formal.clone()]
        ));
        let setter = declaration.setter.as_ref().unwrap();
        assert!(setter.generic_sig.is_none());
        assert!(crate::types::ty_mentions_param(
            setter.params[0],
            &[property_formal.clone()]
        ));
        assert!(crate::types::ty_mentions_param(
            setter.params[1],
            &[property_formal.clone()]
        ));
        assert!(crate::types::ty_mentions_param(
            cp.external_callable(setter_id).unwrap().callable.params[1],
            &[property_formal.clone()]
        ));
    }

    #[test]
    fn enhanced_declared_result_moves_with_the_callable_identity() {
        let cp = Classpath::new(vec![]);
        let owner = type_name("fixture/Enhanced");
        let mut callable =
            generic_callable(owner, "produce", "(Ljava/lang/Object;)Ljava/lang/Object;");
        callable.vararg_elem = Some(callable.ret);
        callable.enhanced_result.declared = Some((owner, callable.ret));
        let identity = cp.intern_external_callable(
            &callable,
            ExternalCallableKind::Member,
            Some(SemanticCallableOwner::Classifier(owner)),
        );
        cp.normalize_external_callable_generic_signature(identity, &mut callable);

        let formal = callable.generic_sig.as_ref().unwrap().formals[0].clone();
        assert!(crate::types::ty_mentions_param(
            callable.vararg_elem.unwrap(),
            &[formal.clone()]
        ));
        let declared = callable.enhanced_result.declared.unwrap().1;
        assert!(crate::types::ty_mentions_param(declared, &[formal.clone()]));
        let stored = cp.external_callable(identity).unwrap();
        assert!(crate::types::ty_mentions_param(
            stored.callable.vararg_elem.unwrap(),
            &[formal.clone()]
        ));
        assert!(crate::types::ty_mentions_param(
            stored.callable.enhanced_result.declared.unwrap().1,
            &[formal]
        ));
    }
}
