//! Normalize dependency declarations into stable provider identities.
//!
//! Classifier, spelling-indexed, property, and cached-inline views can discover the same physical
//! declaration in different orders. This boundary publishes one identity and its complete source
//! parameter/visibility facets before checked FIR carries it to a backend.

use super::JvmLibraries;
use crate::libraries::{
    FnKind, FunctionInfo, LibraryCallable, LibraryMember, PropKind, PropertyInfo,
};

impl JvmLibraries {
    pub(super) fn register_external_callable(&self, callable: &mut LibraryCallable, kind: FnKind) {
        if !matches!(callable.origin, crate::libraries::Origin::Library) {
            return;
        }
        if callable.overridden_call_realizations.is_empty() {
            callable.overridden_call_realizations =
                super::super::mapped_builtin_declarations::overridden_call_realizations_for_declaration(
                    &callable.name,
                    &callable.descriptor,
                    super::super::mapped_builtin_declarations::MappedBuiltinMemberKind::Function,
                )
                .into_boxed_slice();
        }
        if let Some(plan) = callable.inline_body_plan.as_deref_mut() {
            self.register_inline_body_plan_dependencies(plan);
        }
        if let Some(identity) = callable.external_identity {
            self.cp.enrich_external_callable(identity, callable);
            return;
        }
        let kind = match kind {
            FnKind::TopLevel => super::super::classpath::ExternalCallableKind::TopLevel,
            FnKind::Extension => super::super::classpath::ExternalCallableKind::Extension,
            FnKind::Member => super::super::classpath::ExternalCallableKind::Member,
        };
        callable.external_identity = Some(self.cp.intern_external_callable(callable, kind));
    }

    pub(super) fn register_external_inline_member(&self, member: &mut LibraryMember) {
        self.register_external_inline_callable(member, FnKind::Member);
    }

    /// Inline-plan caches are shared by immutable classpath composition, while callable identities
    /// are local to one `Classpath` instance. Always re-home a cached dependency through the
    /// consuming classpath's physical declaration key before the plan crosses the provider boundary.
    pub(super) fn register_external_inline_dependency_callable(
        &self,
        callable: &mut LibraryCallable,
        kind: FnKind,
    ) {
        callable.external_identity = None;
        self.register_external_callable(callable, kind);
    }

    fn register_external_inline_callable(&self, member: &mut LibraryMember, kind: FnKind) {
        let Some(owner) = member.owner else {
            return;
        };
        let mut callable = FunctionInfo::classifier_member(kind, owner, member.clone()).callable;
        self.register_external_inline_dependency_callable(&mut callable, kind);
        member.external_identity = callable.external_identity;
    }

    pub(super) fn register_external_property(&self, property: &mut PropertyInfo) {
        let kind = match property.kind {
            PropKind::TopLevel => FnKind::TopLevel,
            PropKind::Extension => FnKind::Extension,
            PropKind::Member | PropKind::MemberExtension => FnKind::Member,
        };
        property.getter.overridden_call_realizations =
            super::super::mapped_builtin_declarations::overridden_call_realizations_for_declaration(
                &property.name,
                &property.getter.descriptor,
                super::super::mapped_builtin_declarations::MappedBuiltinMemberKind::Property,
            )
            .into_boxed_slice();
        let mut getter_parameter_identities = property.context_parameter_identities.clone();
        let extension = matches!(
            property.kind,
            PropKind::Extension | PropKind::MemberExtension
        );
        if extension && property.getter.params.len() == getter_parameter_identities.len() + 1 {
            getter_parameter_identities.insert(
                property.context_count,
                crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
            );
        }
        assert_eq!(
            property.getter.params.len(),
            getter_parameter_identities.len(),
            "a normalized dependency property getter publishes every parameter identity"
        );
        property.getter.visibility = property.visibility;
        self.register_external_callable(&mut property.getter, kind);
        if let Some(identity) = property.getter.external_identity {
            self.cp.publish_external_callable_parameter_identities(
                identity,
                getter_parameter_identities.into_boxed_slice(),
            );
        }
        if let Some(setter) = &mut property.setter {
            setter.visibility = property.setter_visibility;
            let mut setter_parameter_identities = property.context_parameter_identities.clone();
            if extension && setter.params.len() == setter_parameter_identities.len() + 2 {
                setter_parameter_identities.insert(
                    property.context_count,
                    crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
                );
            }
            setter_parameter_identities.push(property.setter_parameter_name.as_deref().map_or(
                crate::fir::ResolvedParameterIdentity::PropertySetterValue,
                |name| crate::fir::ResolvedParameterIdentity::Source(name.into()),
            ));
            assert_eq!(
                setter.params.len(),
                setter_parameter_identities.len(),
                "a normalized dependency property setter publishes every parameter identity"
            );
            self.register_external_callable(setter, kind);
            if let Some(identity) = setter.external_identity {
                self.cp.publish_external_callable_parameter_identities(
                    identity,
                    setter_parameter_identities.into_boxed_slice(),
                );
            }
        }
        let Some(getter) = property.getter.external_identity else {
            return;
        };
        let setter = property
            .setter
            .as_ref()
            .and_then(|setter| setter.external_identity);
        let identity = self.cp.intern_external_property(
            &property.name,
            getter,
            setter,
            property.compile_time_constant.clone(),
        );
        property.getter.external_property_identity = Some(identity);
        if let Some(setter) = &mut property.setter {
            setter.external_property_identity = Some(identity);
        }
    }

    pub(super) fn register_external_callables(
        &self,
        callables: crate::libraries::Callables,
    ) -> crate::libraries::Callables {
        let (mut functions, mut properties) = callables.into_parts();
        for function in &mut functions.overloads {
            function.callable.visibility = function.visibility;
            function.callable.inline_modifiers = function
                .call_sig
                .inline_modifiers
                .clone()
                .into_boxed_slice();
            let parameter_identities = (function.visibility
                == crate::types::Visibility::Protected)
                .then(|| {
                    function
                        .call_sig
                        .physical_parameter_identities(
                            function.callable.params.len(),
                            function.context_count,
                            (function.is_extension()
                                && function.callable.params.len()
                                    == function.call_sig.parameter_identities.len() + 1)
                                .then_some(function.context_count),
                        )
                        .expect(
                            "a normalized protected dependency function publishes every parameter identity",
                        )
                });
            self.register_external_callable(&mut function.callable, function.kind);
            if let (Some(identity), Some(parameter_identities)) =
                (function.callable.external_identity, parameter_identities)
            {
                self.cp
                    .publish_external_callable_parameter_identities(identity, parameter_identities);
            }
        }
        for property in &mut properties.overloads {
            self.register_external_property(property);
        }
        crate::libraries::Callables::from_parts(functions, properties)
    }
}
