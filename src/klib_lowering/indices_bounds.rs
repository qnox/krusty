//! The member kotlinc's `IndicesHandler` bounds a counted `indices` loop with.
//!
//! A `for` loop over the `indices` of an array reads the array's intrinsic size. Over a
//! `Collection` or a `CharSequence` the bound is the receiver's `size` or `length`, a member read
//! the serialized loop never makes: it holds only the `indices` getter call. The selected
//! declaration of that getter names the receiver kind, and the member is an ordinary dependency
//! declaration, which the closure roots wherever a body loops over such indices and which lowering
//! reads as it reads any dependency property.

use crate::klib_libraries::KlibDeclarationBodies;
use crate::libraries::{IndicesReceiver, KlibBodyCallable, KlibDependencyCallable};
use crate::metadata::id_signature::KlibPublicIdSignature;

use super::decline::KlibBodyDeclineReason;

/// The getter that bounds a loop over the indices `selected` is, when `selected` is the `indices`
/// getter of a receiver whose bound is a member read.
pub(crate) fn bound_member(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Option<KlibDependencyCallable>, KlibBodyDeclineReason> {
    let Some(kind) = super::for_loops::indices_receiver(selected) else {
        return Ok(None);
    };
    bound_getter(kind, &selected.identity().module, bodies)
}

/// The getter that bounds a loop over the indices of a receiver of `kind` in `module`'s producer
/// graph. An array's size is the intrinsic one: how it is read is the lowering's representation
/// decision, so no member bounds it.
fn bound_getter(
    kind: IndicesReceiver,
    module: &str,
    bodies: &KlibDeclarationBodies,
) -> Result<Option<KlibDependencyCallable>, KlibBodyDeclineReason> {
    let Some(class) = kind.bound_class() else {
        return Ok(None);
    };
    bodies
        .public_class_member_getter(
            module,
            &KlibPublicIdSignature::classifier(class),
            kind.bound_property(),
        )
        .map(Some)
        .ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "the receiver of an `indices` loop has no single bounding property".to_owned(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::KlibDeclarationSignature;

    /// The bounding getter of each receiver kind is the declared getter of that kind's
    /// `IndicesHandler` property on its class, and an array's has none.
    #[test]
    fn each_receiver_kind_is_bounded_by_its_declared_getter() {
        let Some(root) = super::super::tests::distribution_root() else {
            return;
        };
        let libraries = super::super::tests::stdlib(&root);
        let bodies = libraries.declaration_bodies();
        let ty = |name: &str| crate::types::Ty::obj_name(crate::types::type_name(name));
        let record = super::super::tests::frozen_function(
            &libraries,
            "kotlin/text",
            "removePrefix",
            Some(ty("kotlin/String")),
            &[ty("kotlin/CharSequence")],
        );
        let module = record
            .klib_body_callable()
            .expect("signed")
            .identity()
            .module
            .clone();
        let getter = |kind| {
            bound_getter(kind, &module, bodies)
                .expect("the stdlib declares the bound")
                .map(|bound| match bound.declaration {
                    KlibDeclarationSignature::Accessor(accessor) => accessor.name().to_owned(),
                    other => panic!("the bound is not an accessor: {other:?}"),
                })
        };
        assert_eq!(getter(IndicesReceiver::Array), None);
        assert_eq!(
            getter(IndicesReceiver::Collection).as_deref(),
            Some("<get-size>")
        );
        assert_eq!(
            getter(IndicesReceiver::CharSequence).as_deref(),
            Some("<get-length>")
        );
    }
}
