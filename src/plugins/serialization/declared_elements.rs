//! The serial elements a serializable source class publishes for subclasses in other files.
//!
//! A file's lowering sees only the classes its own IR holds, so a subclass whose serializable
//! superclass another file of the module declares inherits that superclass's elements through the
//! same [`SerializableClassElements`] record a dependency's metadata provides
//! ([`super::external_parent`]). The plugin publishes it here, from the module's checked
//! declarations, before any file is lowered, and states exactly what its lowering of the class
//! generates: the record exists only for a class whose `write$Self` and deserialization
//! constructor it generates ([`MemberGeneration`]), its elements are the class's stored properties
//! that are not `@Transient`, in declaration order, and an element is optional exactly when its
//! property has a checked default, which the deserialization constructor applies
//! ([`super::property_default`]).

use crate::plugins::FrontendDeclaredClass;
use crate::types::{type_name, SerializableClassElements, SerializableElement};

use super::serial_elements::{is_transient, MemberGeneration};
use super::SERIALIZABLE_FQ;

/// The record for `class`; none for a class whose members the plugin does not generate, and none
/// when an element carries an annotation the record cannot express.
pub(super) fn declared_serializable_elements(
    class: &FrontendDeclaredClass<'_>,
) -> Option<SerializableClassElements> {
    let serializable = class
        .applied_annotations
        .iter()
        .find(|annotation| annotation.annotation == type_name(SERIALIZABLE_FQ));
    let generation = MemberGeneration {
        kind: class.kind,
        is_value: class.is_value,
        serializable: serializable.is_some(),
        custom_serializer: serializable
            .is_some_and(|annotation| annotation.arguments.iter().any(|(name, _)| name == "with")),
    };
    if !generation.generates_members() {
        return None;
    }
    let mut elements = Vec::new();
    for property in class.properties {
        if !property.has_backing_field || is_transient(&property.annotations) {
            continue;
        }
        // A serial name, a serializer of its own or a serial-info annotation changes the
        // element's wire shape, on the property or anywhere in its type, and the record carries
        // none.
        if !property.annotations.is_empty() || property.type_annotated {
            return None;
        }
        elements.push(SerializableElement {
            name: property.name.clone(),
            ty: property.ty,
            optional: property.has_default,
            serial_name: None,
            serializer: None,
            contextual: false,
            type_spelling: crate::spelling::Spelled::default(),
        });
    }
    // The module's checked declarations do not name the type parameters the element types use, so
    // a generic class's record lists none and a subclass in another file declines.
    Some(SerializableClassElements {
        type_parameters: Box::default(),
        elements: elements.into_boxed_slice(),
        superclass: class.superclass,
    })
}
