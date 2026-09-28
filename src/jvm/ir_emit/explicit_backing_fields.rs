//! JVM owner selection for already-checked explicit backing-field property reads.
//!
//! The frontend decides field versus accessor. This module only gives a selected virtual accessor
//! the receiver's exact static JVM owner, matching kotlinc's invocation shape for subclass values.

use super::*;

impl Emitter<'_> {
    /// The virtual owner of an explicit-backing-field getter. kotlinc names the call on the
    /// receiver's static class (`HolderChild.getStamp`), not on the class that declared the field.
    fn explicit_backing_receiver_class(
        &self,
        owner: &str,
        name: &str,
        receiver: Option<crate::ir::ExprId>,
    ) -> Option<String> {
        let class = self
            .ir
            .classes
            .iter()
            .find(|class| class.fq_name_matches(owner))?;
        class
            .properties
            .iter()
            .find(|property| property.name == name && property.storage_ty.is_some())?;
        let receiver = receiver?;
        let static_type = self
            .ir
            .logical_types
            .get(&receiver)
            .copied()
            .unwrap_or_else(|| self.value_ty(receiver));
        let static_name = static_type.non_null().obj_internal()?;
        if class.fq_name_id() == static_name {
            return None;
        }
        Some(static_name.render())
    }

    pub(super) fn retarget_explicit_backing_read(
        &self,
        operation: &PropertyOperation<'_>,
        access: &mut crate::jvm::inline::PropertyAccess,
    ) {
        let Some(receiver_owner) = self.explicit_backing_receiver_class(
            operation.owner,
            operation.name,
            operation.receiver,
        ) else {
            return;
        };
        if let crate::jvm::inline::PropertyAccess::Accessor { owner, .. } = access {
            *owner = receiver_owner;
        }
    }
}
