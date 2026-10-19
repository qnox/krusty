//! Property declaration shapes shared by semantic checking and lowering.

use super::PropDecl;
use crate::types::Visibility;

/// Source name of a setter's value parameter, including the implicit default.
pub(crate) fn setter_param_or_value(param: Option<&String>) -> String {
    param.cloned().unwrap_or_else(|| "value".to_string())
}

/// A FIELD-LESS `companion object` property (`companion object { val ZERO: T get() = … }`): it IS
/// its accessors, so there is no static to hoist onto the outer class — kotlinc emits only `getX`
/// (plus `setX` for a `var`) on `C$Companion`, and that is what the companion synthesis builds.
///
/// A `var` requires a BODIED setter for the same reason a `val` requires a getter: with no backing
/// field a default setter would have nothing to write. A getter that reads `field`, an initializer,
/// an explicit backing field, a delegate, or `const` all mean a real static exists, so those keep
/// the plain companion-property path.
pub fn is_computed_companion_prop(property: &PropDecl) -> bool {
    property.receiver.is_none()
        && !property.is_lateinit
        && !property.is_const
        && property.init.is_none()
        && property.delegate.is_none()
        && property.explicit_backing_field.is_none()
        && property.getter.is_some()
        && !property.getter_reads_field
        && if property.is_var {
            // A `private set` narrows only the SETTER, and the accessor synthesis emits an
            // unconditionally public `setX` — accepting one would let a write through that kotlinc
            // rejects, so those keep the rejection path until the narrowed visibility is modeled.
            property.setter.as_ref().is_some_and(|setter| {
                setter.body.is_some() && setter.visibility != Some(Visibility::Private)
            })
        } else {
            property.setter.is_none()
        }
}

/// A field-less member property whose value is entirely supplied by its accessor body.
///
/// This is a source declaration shape shared by semantic checking and IR construction. Keeping it
/// beside [`PropDecl`] prevents those phases from independently deciding which declarations own
/// instance storage.
pub fn is_computed_member_prop(property: &PropDecl) -> bool {
    property.receiver.is_none()
        && !property.is_lateinit
        && property.init.is_none()
        && property.getter.is_some()
        && (!property.is_var
            || property
                .setter
                .as_ref()
                .is_some_and(|setter| setter.body.is_some()))
        && !property.getter_reads_field
}

/// Whether this member-property declaration contributes one instance-storage slot.
pub fn has_instance_backing_field(property: &PropDecl) -> bool {
    !property.is_const && has_backing_field(property)
}

/// Whether this property declaration can own the backing field selected by an explicit `@field:`
/// annotation. Static `const` storage counts; delegates and computed/extension/context properties
/// have other realizations and therefore do not.
pub fn has_backing_field(property: &PropDecl) -> bool {
    !is_computed_member_prop(property)
        && !property.is_abstract
        && property.delegate.is_none()
        && property.receiver.is_none()
        && property.context_params.is_empty()
}
