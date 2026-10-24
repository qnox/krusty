//! What `field` names inside a property accessor.
//!
//! kotlinc binds `field` to the property's backing field before every member or top-level
//! property, even an anonymous object's own `field` declared inside the accessor; only a lexical
//! local or parameter shadows it.

use super::*;

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
