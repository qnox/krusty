//! What `field` names inside a property accessor.
//!
//! kotlinc binds `field` to the property's backing field before every member or top-level
//! property, even an anonymous object's own `field` declared inside the accessor; only a lexical
//! local or parameter shadows it. A getter whose property declares neither a type nor an
//! initializer infers the property's type from itself and has no backing field `field` at all.

use super::*;

/// Whether the getter of `property` binds `field` to the backing field.
pub(super) fn getter_declares_field(property: &PropDecl) -> bool {
    crate::ast::getter_declares_field(property.declared_ty().is_some(), property.init.is_some())
}

impl Checker<'_> {
    /// Whether `name`, read or written in an accessor, names the backing field rather than the
    /// binding of `origin`.
    pub(super) fn backing_field_shadows(&self, name: &str, origin: ReceiverFnValueOrigin) -> bool {
        name == "field"
            && self.field_ty.is_some()
            && !matches!(
                origin,
                ReceiverFnValueOrigin::Local | ReceiverFnValueOrigin::ClassStorage(_)
            )
    }
}
