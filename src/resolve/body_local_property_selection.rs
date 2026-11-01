//! Access-specific selection of properties declared by a classifier in the active bounded body.
//!
//! These declarations are not yet present in the immutable module provider. The overlay still
//! consumes the same provider-normalized accessor availability as ordinary member selection.

use super::*;

#[cfg(test)]
mod tests;

impl CheckedLocalProperty {
    fn readable(&self) -> bool {
        !self.deprecated_hidden && self.accessor_availability.getter
    }

    fn write_selectable(&self) -> bool {
        !self.deprecated_hidden && (!self.mutable || self.accessor_availability.setter)
    }
}

impl Checker<'_> {
    /// Install one deferred explicit body-property header before nested classifiers and sibling
    /// methods are checked. The active overlay is the same normalized candidate boundary used by
    /// ordinary selection; no declaration is published to the immutable module index here.
    /// Returns whether an ordinary (non-extension) property was added to `properties`.
    pub(super) fn register_deferred_explicit_body_property(
        &mut self,
        scope: &CheckerScope<'_>,
        owner: Option<TypeName>,
        property: &PropDecl,
        source_member: crate::libraries::SourceMember,
        stable_declaration: Option<crate::fir::DeclarationId>,
        interface: bool,
        properties: &mut Vec<ScopedProperty>,
    ) -> bool {
        if !self.has_deferred_local_header(stable_declaration) {
            return false;
        }
        let (Some(owner), Some(declared)) = (owner, property.declared_ty()) else {
            return false;
        };
        if let Some(receiver_ref) = property.receiver.as_ref() {
            let (deprecated_hidden, accessor_availability) =
                self.checked_property_availability(stable_declaration, Some(property));
            let header_scope = scope.child(ScopeKind::Function { receiver: None });
            let visible = header_scope.visible_tparams();
            let type_parameters = visible
                .symbolic_extended_with(
                    &property.type_params,
                    &property.type_param_bounds,
                    &|name| self.select_classifier(&header_scope, name).found(),
                )
                .alpha_renamed_declaration(
                    &property.type_params,
                    self.compilation_id,
                    self.file_index,
                    property.span.lo,
                );
            header_scope.declare_tparams(&property.type_params, &type_parameters, |name| {
                property.reified_type_params.contains(name)
            });
            let receiver = self.check_declaration_type(&header_scope, receiver_ref);
            let result = self.check_declaration_type(&header_scope, declared);
            let context_params = property
                .context_params
                .iter()
                .map(|parameter| {
                    semantic_value_parameter_ty(
                        self.check_declaration_type(&header_scope, &parameter.ty),
                        parameter.is_vararg,
                    )
                })
                .collect::<Vec<_>>();
            let type_params = property
                .type_params
                .iter()
                .map(|source| {
                    type_parameters
                        .bound(source)
                        .ty_param_name()
                        .map(crate::types::TyParamName::new)
                })
                .collect::<Option<Vec<_>>>();
            let formal_bounds = property
                .type_params
                .iter()
                .map(|source| {
                    property
                        .type_param_bounds
                        .iter()
                        .filter(|(parameter, _)| parameter == source)
                        .map(|(_, bound)| self.check_declaration_type(&header_scope, bound))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let type_param_bounds = formal_bounds
                .iter()
                .map(|bounds| {
                    bounds
                        .first()
                        .copied()
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                })
                .collect::<Vec<_>>();
            let Some(type_params) = type_params else {
                return false;
            };
            if std::iter::once(receiver)
                .chain(std::iter::once(result))
                .chain(context_params.iter().copied())
                .chain(formal_bounds.iter().flatten().copied())
                .any(|ty| ty.mentions_error() || ty.mentions_pending())
            {
                return false;
            }
            self.record_checked_local_member_extension_property(
                owner,
                &property.name,
                member_extension_properties::MemberExtPropSig {
                    receiver,
                    ret: result,
                    context_params,
                    context_param_names: property
                        .context_params
                        .iter()
                        .map(|parameter| parameter.name.clone())
                        .collect(),
                    context_parameter_identities: Vec::new(),
                    type_params,
                    type_param_bounds,
                    formal_bounds,
                    is_var: property.is_var,
                    visibility: property.visibility,
                    setter_visibility: property.is_var.then(|| {
                        property
                            .setter
                            .as_ref()
                            .and_then(|setter| setter.visibility)
                            .unwrap_or(property.visibility)
                    }),
                    annotations: Vec::new(),
                    deprecated_hidden,
                    accessor_availability,
                    stable_declaration,
                    source_member: Some(source_member),
                    getter: None,
                    setter: None,
                },
            );
            return false;
        }
        // Context-dependent and generic property candidates require their own argument-selection
        // shape. They never enter the receiver-less overlay until that normalized shape exists.
        if !property.context_params.is_empty() || !property.type_params.is_empty() {
            return false;
        }
        let checked = self.check_declaration_type(scope, declared);
        if checked.mentions_error() || checked.mentions_pending() {
            return false;
        }
        self.property_decl_types
            .insert((property.span.lo, property.span.hi), checked);
        self.record_checked_local_property(
            owner,
            property,
            source_member,
            stable_declaration,
            checked,
            interface,
        );
        let checked_property = ScopedProperty {
            name: property.name.clone(),
            ty: checked,
            owner_storage_ty: None,
            is_var: property.is_var,
            owner,
            dispatch_receiver_identity: self
                .implicit_receivers(scope)
                .into_iter()
                .find(|receiver| receiver.ty.obj_internal() == Some(owner))
                .map(|receiver| receiver.identity),
            class_storage: None,
            enum_entry_property: None,
            source_visible: true,
            source_member: Some(source_member),
            stable_declaration,
        };
        let index = if let Some(index) = stable_declaration.and_then(|declaration| {
            properties
                .iter()
                .position(|candidate| candidate.stable_declaration == Some(declaration))
        }) {
            properties[index] = checked_property;
            index
        } else {
            properties.push(checked_property);
            properties.len() - 1
        };
        let checked = &properties[index];
        self.declare_scoped_property(scope, checked, false);
        true
    }

    pub(super) fn record_checked_local_member_extension_property(
        &mut self,
        owner: TypeName,
        name: &str,
        property: member_extension_properties::MemberExtPropSig,
    ) {
        let declarations = self
            .checked_local_member_extension_properties
            .entry(owner)
            .or_default()
            .entry(name.to_owned())
            .or_default();
        if let Some(existing) = declarations
            .iter_mut()
            .find(|existing| existing.stable_declaration == property.stable_declaration)
        {
            *existing = property;
        } else {
            declarations.push(property);
        }
    }

    /// Add the checked constructor-property view required while a body-local classifier has no
    /// provider header of its own. An active declaration may enter this overlay only when the
    /// exact stable property role is explicitly deferred; a finalized missing provider fact is a
    /// publication failure. Legacy checking retains its pre-stable-inventory path.
    pub(super) fn append_authoritative_constructor_property_overlay(
        &self,
        scope: &CheckerScope<'_>,
        properties: &mut Vec<ScopedProperty>,
        owner: TypeName,
        property: &PropParam,
        source_member: crate::libraries::SourceMember,
        stable_declaration: Option<crate::fir::DeclarationId>,
        stable_fact: Option<(crate::fir::DeclarationId, bool)>,
        checked: Ty,
    ) -> bool {
        let authorized = self.active_declarations.is_none()
            || stable_fact.is_some_and(|(declaration, finalized)| {
                !finalized && Some(declaration) == stable_declaration
            });
        if !authorized {
            return false;
        }
        properties.push(ScopedProperty {
            name: property.name.clone(),
            ty: checked,
            owner_storage_ty: None,
            is_var: property.is_var,
            owner,
            dispatch_receiver_identity: self
                .implicit_receivers(scope)
                .into_iter()
                .find(|receiver| receiver.ty.obj_internal() == Some(owner))
                .map(|receiver| receiver.identity),
            class_storage: None,
            enum_entry_property: None,
            source_visible: true,
            source_member: Some(source_member),
            stable_declaration,
        });
        true
    }

    fn record_checked_local_property_shape(
        &mut self,
        owner: TypeName,
        name: &str,
        has_receiver: bool,
        mutable: bool,
        visibility: Visibility,
        source_member: crate::libraries::SourceMember,
        stable_declaration: Option<crate::fir::DeclarationId>,
        ty: Ty,
        interface: bool,
    ) {
        if has_receiver || ty.mentions_error() || ty.mentions_pending() {
            return;
        }
        let (deprecated_hidden, accessor_availability) =
            self.checked_property_availability(stable_declaration, None);
        self.checked_local_properties.insert(
            (owner, name.to_owned()),
            CheckedLocalProperty {
                ty,
                owner,
                mutable,
                visibility,
                interface,
                source_member,
                stable_declaration,
                deprecated_hidden,
                accessor_availability,
            },
        );
    }

    /// Normalize source availability while the stable property and accessor identities are in
    /// hand. Both ordinary and member-extension overlays consume this fact without reopening
    /// syntax or treating active declarations differently from provider declarations.
    fn checked_property_availability(
        &self,
        stable_declaration: Option<crate::fir::DeclarationId>,
        active_property: Option<&PropDecl>,
    ) -> (bool, crate::libraries::PropertyAccessorAvailability) {
        let published_hidden = |declaration| {
            self.resolved_index.is_some_and(|index| {
                index
                    .declaration_applied_annotations(declaration)
                    .iter()
                    .any(crate::types::ResolvedAnnotation::is_deprecated_hidden)
            })
        };
        let active_hidden = |annotation: &crate::ast::AnnotationRef| {
            self.applied_annotations
                .get(&(annotation.span.lo, annotation.span.hi))
                .is_some_and(|application| application.facts.deprecated_hidden)
        };
        let deprecated_hidden = stable_declaration.is_some_and(published_hidden)
            || active_property
                .is_some_and(|property| property.annotations.iter().any(active_hidden));
        let accessor_available = |sibling, active: Option<&[crate::ast::AccessorAnnotation]>| {
            stable_declaration
                .and_then(|declaration| {
                    self.resolved_index?.owned_declaration(
                        declaration,
                        crate::fir::DeclarationKind::Accessor,
                        sibling,
                    )
                })
                .is_none_or(|accessor| !published_hidden(accessor))
                && active.is_none_or(|annotations| {
                    !annotations
                        .iter()
                        .any(|entry| active_hidden(&entry.annotation))
                })
        };
        (
            deprecated_hidden,
            crate::libraries::PropertyAccessorAvailability {
                getter: accessor_available(
                    0,
                    active_property.map(|property| property.getter_annotations.as_slice()),
                ),
                setter: accessor_available(
                    1,
                    active_property
                        .and_then(|property| property.setter.as_ref())
                        .map(|setter| setter.annotations.as_slice()),
                ),
            },
        )
    }

    pub(super) fn record_checked_local_property(
        &mut self,
        owner: TypeName,
        property: &PropDecl,
        source_member: crate::libraries::SourceMember,
        stable_declaration: Option<crate::fir::DeclarationId>,
        ty: Ty,
        interface: bool,
    ) {
        self.record_checked_local_property_shape(
            owner,
            &property.name,
            property.receiver.is_some(),
            property.is_var,
            property.visibility,
            source_member,
            stable_declaration,
            ty,
            interface,
        );
    }

    pub(super) fn record_checked_local_constructor_property(
        &mut self,
        owner: TypeName,
        property: &PropParam,
        source_member: crate::libraries::SourceMember,
        stable_declaration: Option<crate::fir::DeclarationId>,
        ty: Ty,
        interface: bool,
    ) {
        if !property.is_property {
            return;
        }
        self.record_checked_local_property_shape(
            owner,
            &property.name,
            false,
            property.is_var,
            property.visibility,
            source_member,
            stable_declaration,
            ty,
            interface,
        );
    }

    /// The first available property declaration published by a classifier in the active bounded
    /// body. Reads and writes use the same overlay but independently select their accessor.
    pub(super) fn checked_body_local_property(
        &self,
        receiver: Ty,
        name: &str,
    ) -> Option<&CheckedLocalProperty> {
        self.checked_body_local_property_where(receiver, name, CheckedLocalProperty::readable)
    }

    pub(super) fn checked_body_local_property_for_write(
        &self,
        receiver: Ty,
        name: &str,
    ) -> Option<&CheckedLocalProperty> {
        self.checked_body_local_property_where(
            receiver,
            name,
            CheckedLocalProperty::write_selectable,
        )
    }

    fn checked_body_local_property_where(
        &self,
        receiver: Ty,
        name: &str,
        available: impl Fn(&CheckedLocalProperty) -> bool,
    ) -> Option<&CheckedLocalProperty> {
        std::iter::once((receiver.non_null(), false))
            .chain(
                self.body_local_supertypes(receiver)
                    .into_iter()
                    .map(|supertype| (supertype, true)),
            )
            .filter_map(|(candidate, inherited)| {
                let owner = candidate.kotlin_class_internal()?;
                let property = self
                    .checked_local_properties
                    .get(&(owner, name.to_string()))?;
                (available(property)
                    && (!inherited
                        || crate::symbol_resolver::member_is_inheritable(property.visibility)))
                .then_some(property)
            })
            .next()
    }
}
