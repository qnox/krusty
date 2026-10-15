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
