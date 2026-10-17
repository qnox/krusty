//! Access-specific selection of properties declared by a classifier in the active bounded body.
//!
//! These declarations are not yet present in the immutable module provider. The overlay still
//! consumes the same provider-normalized accessor availability as ordinary member selection.

use super::*;

impl CheckedLocalProperty {
    fn readable(&self) -> bool {
        !self.deprecated_hidden && self.accessor_availability.getter
    }

    fn write_selectable(&self) -> bool {
        !self.deprecated_hidden && (!self.mutable || self.accessor_availability.setter)
    }
}

impl Checker<'_> {
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
        let hidden = |declaration| {
            self.resolved_index.is_some_and(|index| {
                index
                    .declaration_applied_annotations(declaration)
                    .iter()
                    .any(crate::types::ResolvedAnnotation::is_deprecated_hidden)
            })
        };
        let deprecated_hidden = stable_declaration.is_some_and(hidden);
        let accessor_available = |sibling| {
            stable_declaration
                .and_then(|declaration| {
                    self.resolved_index?.owned_declaration(
                        declaration,
                        crate::fir::DeclarationKind::Accessor,
                        sibling,
                    )
                })
                .is_none_or(|accessor| !hidden(accessor))
        };
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
                accessor_availability: crate::libraries::PropertyAccessorAvailability {
                    getter: accessor_available(0),
                    setter: accessor_available(1),
                },
            },
        );
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
