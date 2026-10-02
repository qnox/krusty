//! Selecting the field-visible type and storage operation for explicit backing-field reads.
//!
//! The property keeps its public type outside its declaring class. Inside that class, an exact
//! owner instance exposes the narrower field type; nested classes and inherited properties still
//! use the getter. These decisions consume stable receiver and owner identities from the scope
//! tower rather than recovering them from source spelling.

use super::*;

impl Checker<'_> {
    /// A qualified read sees an explicit backing field's type when a lexical binding of this
    /// property carries that field and the receiver's static type is exactly the declaring class.
    /// The field is loaded only when this expression is compiled into that class (`other.a` and
    /// `this.a` are `getfield`; a local function and a lambda are too). A nested or inner class
    /// keeps the field's type but calls the getter, which returns the public type and is then
    /// checked back. A subclass value and an inherited property stay on the public type and getter.
    pub(super) fn qualified_owner_storage_type(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        receiver_ty: Ty,
        name: &str,
    ) -> Option<Ty> {
        let binding = scope.ancestors().find_map(|rung| {
            let local = rung.own_binding(name, Ns::Value)?.value()?;
            match local.origin {
                ReceiverFnValueOrigin::DispatchProperty {
                    owner,
                    owner_storage: true,
                    ..
                } if receiver_ty.non_null().obj_internal() == Some(owner) => Some(local),
                _ => None,
            }
        })?;
        let ReceiverFnValueOrigin::DispatchProperty {
            declared_ty,
            owner: binding_owner,
            receiver_identity,
            ..
        } = binding.origin
        else {
            return None;
        };
        let Some(ExprLowering::MemberPropertyRead {
            owner,
            owner_storage,
            ..
        }) = self.expr_lowers.get_mut(&expression)
        else {
            return None;
        };
        if *owner != binding_owner {
            return None;
        }
        let compiled_into_owner = scope
            .innermost_class_receiver_identity()
            .is_none_or(|class_identity| class_identity == receiver_identity);
        if compiled_into_owner {
            *owner_storage = true;
        }
        (binding.ty != declared_ty).then_some(binding.ty)
    }

    /// The narrower backing-field type visible at the declaration site, if this scope belongs to
    /// the declaring class. A recursively collected inherited property never satisfies that exact
    /// owner check.
    pub(super) fn visible_owner_storage_type(
        &self,
        scope: &CheckerScope<'_>,
        property: &ScopedProperty,
        owner_storage_visible: bool,
    ) -> Option<Ty> {
        if !owner_storage_visible {
            return None;
        }
        let declaring_class = scope
            .innermost_class_receiver_identity()
            .and_then(|identity| {
                self.implicit_receivers(scope)
                    .into_iter()
                    .find(|receiver| receiver.identity == identity)
                    .and_then(|receiver| receiver.ty.non_null().obj_internal())
            });
        if declaring_class != Some(property.owner) {
            return None;
        }
        // While the field initializer is being determined, its type is the pending marker. The
        // property's already-known public type remains usable, so do not leak that marker.
        property
            .owner_storage_ty
            .filter(|storage| !storage.mentions_pending())
    }
}
