//! Normalize dependency declarations into stable provider identities.
//!
//! Classifier, spelling-indexed, property, and cached-inline views can discover the same target
//! declaration in different orders. This boundary publishes one identity for the joined physical
//! realization and semantic declaration owner, plus its complete source parameter/visibility
//! facets, before checked FIR carries it to a backend.

use super::JvmLibraries;
use crate::libraries::{
    FnKind, FunctionInfo, LibraryCallable, LibraryMember, PropKind, PropertyInfo,
};
use crate::types::{SemanticCallableOwner, TypeName};

impl JvmLibraries {
    pub(super) fn register_external_constructor(
        &self,
        owner: TypeName,
        member: &mut LibraryMember,
    ) {
        let callable = LibraryCallable::constructor(owner, member);
        let declaration_owner = Some(SemanticCallableOwner::Classifier(owner));
        let identity = member
            .external_identity
            .filter(|identity| {
                self.cp
                    .external_callable(*identity)
                    .is_some_and(|realization| realization.declaration_owner == declaration_owner)
            })
            .map(|identity| {
                self.cp.enrich_external_callable(identity, &callable);
                identity
            })
            .unwrap_or_else(|| {
                self.cp.intern_external_callable(
                    &callable,
                    super::super::classpath::ExternalCallableKind::Constructor,
                    declaration_owner,
                )
            });
        member.external_identity = Some(identity);
    }

    pub(super) fn register_external_callable(
        &self,
        callable: &mut LibraryCallable,
        kind: FnKind,
        declaration_owner: Option<SemanticCallableOwner>,
    ) {
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
        // A declaration discovered through more than one provider view keeps the exact semantic
        // owner published by the view that actually declared it. In particular, a Java static
        // field can be reached through both its classifier and package lookup, but it remains a
        // classifier declaration; re-homing it as a package callable would make its field
        // descriptor look like a method descriptor to the backend.
        let declaration_owner = callable.declaration_owner.or(declaration_owner);
        callable.declaration_owner = declaration_owner;
        if let Some(identity) = callable.external_identity {
            if self
                .cp
                .external_callable(identity)
                .is_some_and(|realization| realization.declaration_owner == declaration_owner)
            {
                self.cp.enrich_external_callable(identity, callable);
                return;
            }
            // A partial provider view can carry an identity interned before its exact semantic
            // owner was known. Re-home it under the complete declaration key instead of mutating
            // the meaning of an identity that checked FIR may already hold.
            callable.external_identity = None;
        }
        let kind = match kind {
            FnKind::TopLevel => super::super::classpath::ExternalCallableKind::TopLevel,
            FnKind::Extension => super::super::classpath::ExternalCallableKind::Extension,
            FnKind::Member => super::super::classpath::ExternalCallableKind::Member,
        };
        callable.external_identity = Some(self.cp.intern_external_callable(
            callable,
            kind,
            declaration_owner,
        ));
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
        let declaration_owner = callable.declaration_owner;
        callable.external_identity = None;
        self.register_external_callable(callable, kind, declaration_owner);
    }

    fn register_external_inline_callable(&self, member: &mut LibraryMember, kind: FnKind) {
        let Some(owner) = member.owner else {
            return;
        };
        let mut callable = FunctionInfo::classifier_member(kind, owner, member.clone()).callable;
        callable.external_identity = None;
        self.register_external_callable(
            &mut callable,
            kind,
            Some(SemanticCallableOwner::Classifier(owner)),
        );
        member.external_identity = callable.external_identity;
    }

    pub(super) fn register_external_property(
        &self,
        property: &mut PropertyInfo,
        declaration_owner: Option<SemanticCallableOwner>,
    ) {
        property.metadata_constant_read = self.java_field_is_metadata_constant_read(property);
        // A JavaBean projection reuses the Java methods' declarations; it does not declare Kotlin
        // accessors with generated property-parameter roles. The function provider publishes those
        // methods' exact parameter identities before this projection is registered.
        let declares_accessors = !property.accessor_derived();
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
        self.register_external_callable(&mut property.getter, kind, declaration_owner);
        if declares_accessors {
            if let Some(identity) = property.getter.external_identity {
                self.cp.publish_external_callable_parameter_identities(
                    identity,
                    getter_parameter_identities.into_boxed_slice(),
                );
            }
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
            self.register_external_callable(setter, kind, declaration_owner);
            if declares_accessors {
                if let Some(identity) = setter.external_identity {
                    self.cp.publish_external_callable_parameter_identities(
                        identity,
                        setter_parameter_identities.into_boxed_slice(),
                    );
                }
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
        declaration_owner: Option<SemanticCallableOwner>,
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
            self.register_external_callable(
                &mut function.callable,
                function.kind,
                declaration_owner,
            );
            if let (Some(identity), Some(parameter_identities)) =
                (function.callable.external_identity, parameter_identities)
            {
                self.cp
                    .publish_external_callable_parameter_identities(identity, parameter_identities);
            }
        }
        for property in &mut properties.overloads {
            self.register_external_property(property, declaration_owner);
        }
        crate::libraries::Callables::from_parts(functions, properties)
    }

    /// A Java final field with no compile-time payload. Kotlin metadata still marks a `val`
    /// initialized from it `HAS_CONSTANT`. A Kotlin `@JvmField` lives on a class that has metadata
    /// and is not one of these, and a `const` or JLS constant already publishes its payload.
    fn java_field_is_metadata_constant_read(&self, property: &PropertyInfo) -> bool {
        if property.producer != crate::libraries::PropertyProducer::Field
            || property.setter.is_some()
            || property.is_const
            || property.compile_time_constant.is_some()
        {
            return false;
        }
        self.cp
            .find_name(property.owner)
            .is_some_and(|class| !class.meta.is_present())
    }
}
