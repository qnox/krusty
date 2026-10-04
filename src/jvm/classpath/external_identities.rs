//! Stable dependency declaration identities and their provider-owned JVM realizations.
//!
//! Classpath queries can discover a physical declaration through a partial spelling index before
//! its complete classifier metadata is materialized. This registry interns the physical identity
//! once and monotonically enriches that same record; consumers never repeat lookup by spelling.

use super::{
    Classpath, ExternalCallableKind, ExternalCallableRealization, ExternalPropertyRealization,
};
use crate::libraries::LibraryCallable;
use crate::types::{Ty, TypeName};

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
}

impl Classpath {
    /// Intern one provider-normalized callable and return the stable identity passed through FIR.
    /// The key is the exact physical declaration, never a source lookup key.
    pub(crate) fn intern_external_callable(
        &self,
        callable: &LibraryCallable,
        kind: ExternalCallableKind,
    ) -> crate::fir::ExternalCallableId {
        let key = ExternalCallableKey {
            owner: callable.owner,
            name: callable.physical_name().to_string(),
            descriptor: callable.descriptor.clone(),
            physical_params: callable.physical_params.clone(),
            physical_ret: callable.physical_ret,
            default_call: callable.default_call,
            kind,
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
        let declaration_package = matches!(
            kind,
            ExternalCallableKind::TopLevel | ExternalCallableKind::Extension
        )
        .then(|| callable.owner.parent().unwrap_or(TypeName::ROOT));
        callables.push(ExternalCallableRealization {
            callable: stored,
            kind,
            declaration_package,
            parameter_identities: Box::new([]),
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
        let identity = cp.intern_external_callable(&first, ExternalCallableKind::Member);

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
            cp.intern_external_callable(&enriched, ExternalCallableKind::Member),
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
}
