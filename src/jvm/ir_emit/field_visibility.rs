//! The JVM access flags of a class's backing fields, where kotlinc publishes a field beyond
//! `private`, and of the default setter whose visibility a `lateinit` field shares.

use crate::ir::IrClass;
use crate::types::Visibility;

/// Is this property declared `@JvmField`? The annotation replaces the property's JVM realization
/// wholesale: kotlinc emits NO `getX()`/`setX()` for it and gives the backing field the PROPERTY's
/// declared visibility, so every read and write — inside the class and out — is a field access, and
/// the `@Metadata` record describes only the field.
///
/// Read off the resolved application rather than the spelling: `@JvmField` reaches the FIELD use
/// site by its own declared `@Target` (see `class_field_annotations`), so it is already interned
/// here under its exact identity, and an unrelated user annotation that happens to be spelled
/// `JvmField` resolves to a different one.
///
/// A companion is excluded here because the companion-storage pass hoists its field to the enclosing
/// classifier. A named object's `@JvmField`, by contrast, is a public static on that object class and
/// uses this rule together with the object's backend-selected static storage.
pub(super) fn is_jvm_field(c: &IrClass, property: &str) -> bool {
    !c.is_companion && c.property_has_jvm_field(property)
}

/// The access a backing field publishes when it is not simply private: a `@JvmField` field takes
/// its property's visibility, and a `lateinit` one its setter's, since kotlinc exposes the field of
/// a `lateinit var` as the property's storage (`internal` has no JVM spelling and is public).
pub(super) fn jvm_field_visibility(c: &IrClass, field_index: usize) -> Option<u16> {
    let field = &c.fields[field_index];
    let visibility = if is_jvm_field(c, &field.name) {
        c.properties
            .iter()
            .find(|declaration| declaration.name == field.name)?
            .visibility
    } else if field.is_lateinit() {
        let property = c
            .properties
            .iter()
            .find(|declaration| declaration.backing_field == Some(field_index as u32))?;
        property.setter_visibility
    } else {
        return None;
    };
    Some(match visibility {
        Visibility::Protected => 0x0004,
        Visibility::Private => 0x0002,
        _ => 0x0001,
    })
}

/// A default setter's access: `private set` and `protected set` narrow only the setter, which keeps
/// that declaration fact rather than widen to its property's visibility. Only an overridable
/// property's setter drops `final`, and a private one never does. A setter that merely shares a
/// protected property's visibility stays public like that property's getter: publishing both
/// accessors `protected` waits on the accessors a subclass in another package reaches them by.
pub(super) fn default_setter_access(
    setter_visibility: Visibility,
    property_visibility: Visibility,
    overridable: bool,
) -> u16 {
    let access = match setter_visibility {
        Visibility::Private => return 0x0012,
        Visibility::Protected if property_visibility != Visibility::Protected => 0x0004,
        _ => 0x0001,
    };
    if overridable {
        access
    } else {
        access | 0x0010
    }
}
