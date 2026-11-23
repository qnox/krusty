//! Which properties of a class store a backing field, decided once from declaration flags.
//!
//! Common lowering appends a property's backing field to its class from this answer, and a backend
//! that lays out a class declared in another file reads the same answer from the header, so the
//! two can never disagree about which properties store a field.

use super::DeclarationFlags;

/// Whether a property declared with `property` flags in a classifier with `owner` flags stores a
/// backing field of its own.
pub(crate) fn property_stores_field(owner: DeclarationFlags, property: DeclarationFlags) -> bool {
    !owner.has(DeclarationFlags::INTERFACE)
        && !property.has(DeclarationFlags::DELEGATED)
        && (property.has(DeclarationFlags::PROPERTY_PARAMETER)
            || property.has(DeclarationFlags::HAS_INITIALIZER)
            || property.has(DeclarationFlags::LATEINIT)
            || property.has(DeclarationFlags::EXPLICIT_BACKING_FIELD)
            || property.has(DeclarationFlags::GETTER_READS_BACKING_FIELD)
            || (!property.has(DeclarationFlags::CUSTOM_GETTER)
                && !property.has(DeclarationFlags::ABSTRACT)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(names: &[u64]) -> DeclarationFlags {
        names
            .iter()
            .fold(DeclarationFlags::default(), |all, &flag| {
                all.with(flag, true)
            })
    }

    #[test]
    fn an_initialized_property_of_a_class_stores_a_field() {
        assert!(property_stores_field(
            DeclarationFlags::default(),
            flags(&[DeclarationFlags::HAS_INITIALIZER])
        ));
    }

    #[test]
    fn an_interface_property_never_stores_a_field() {
        assert!(!property_stores_field(
            flags(&[DeclarationFlags::INTERFACE]),
            flags(&[DeclarationFlags::HAS_INITIALIZER])
        ));
    }

    #[test]
    fn a_delegated_property_stores_its_delegate_not_a_backing_field() {
        assert!(!property_stores_field(
            DeclarationFlags::default(),
            flags(&[
                DeclarationFlags::DELEGATED,
                DeclarationFlags::HAS_INITIALIZER
            ])
        ));
    }

    #[test]
    fn a_custom_getter_without_a_backing_read_stores_nothing() {
        assert!(!property_stores_field(
            DeclarationFlags::default(),
            flags(&[DeclarationFlags::CUSTOM_GETTER])
        ));
        assert!(property_stores_field(
            DeclarationFlags::default(),
            flags(&[
                DeclarationFlags::CUSTOM_GETTER,
                DeclarationFlags::GETTER_READS_BACKING_FIELD
            ])
        ));
    }

    #[test]
    fn an_abstract_property_stores_nothing_unless_it_is_a_constructor_parameter() {
        assert!(!property_stores_field(
            DeclarationFlags::default(),
            flags(&[DeclarationFlags::ABSTRACT])
        ));
    }
}
