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

/// A declared backing field's JVM access. Its visibility maps from the platform-neutral one: a
/// `private` field is `ACC_PRIVATE` (the default: Kotlin backing fields are reached via accessors)
/// and a non-private one `ACC_PUBLIC`, unless [`jvm_field_visibility`] publishes it otherwise.
/// Compiler-generated storage is `ACC_SYNTHETIC`, as kotlinc marks `$$delegate_N`.
pub(super) fn declared_field_access(c: &IrClass, field_index: usize, is_static: bool) -> u16 {
    let field = &c.fields[field_index];
    jvm_field_visibility(c, field_index).unwrap_or(if field.is_private() { 0x0002 } else { 0x0001 })
        | if field.is_final() { 0x0010 } else { 0 }
        | if is_static { 0x0008 } else { 0 }
        | if field.is_compiler_generated() {
            0x1000
        } else {
            0
        }
}

/// A default accessor's JVM access. Visibility is an exact checked declaration fact; the backend
/// only maps it to classfile flags. Private accessors remain final, while an open property lets a
/// public/protected accessor dispatch virtually.
pub(super) fn default_accessor_access(visibility: Visibility, overridable: bool) -> u16 {
    let access = match visibility {
        Visibility::Private => return 0x0012,
        Visibility::Protected => 0x0004,
        Visibility::Internal | Visibility::Public => 0x0001,
        Visibility::PackagePrivate => 0x0000,
    };
    if overridable {
        access
    } else {
        access | 0x0010
    }
}

/// Whether a backing field publishes a nullability annotation. The constructor prefix's fields (the
/// outer instance, lexical captures) and other compiler-generated storage (a `$$delegate_N`) are
/// the compiler's own, and kotlinc annotates no synthetic declaration.
pub(super) fn publishes_field_nullability(c: &IrClass, field_index: usize) -> bool {
    field_index >= c.constructor_prefix_count as usize
        && !c.fields[field_index].is_compiler_generated()
}
