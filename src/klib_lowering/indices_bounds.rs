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
use crate::types::wk;

use super::decline::KlibBodyDeclineReason;

/// The property read of the receiver kind `kind` that bounds its indices, and the class
/// declaring it. An array's size is the intrinsic one, so no member bounds it.
fn bound(kind: IndicesReceiver) -> Option<(crate::types::TypeName, &'static str)> {
    match kind {
        IndicesReceiver::Array => None,
        IndicesReceiver::Collection => Some((wk::collection(), wk::SIZE)),
        IndicesReceiver::CharSequence => Some((wk::char_sequence(), wk::CHAR_SEQUENCE_LENGTH)),
    }
}

/// The getter that bounds a loop over the indices `selected` is, when `selected` is the `indices`
/// getter of a receiver whose bound is a member read.
pub(crate) fn bound_member(
    selected: KlibBodyCallable<'_>,
    bodies: &KlibDeclarationBodies,
) -> Result<Option<KlibDependencyCallable>, KlibBodyDeclineReason> {
    let Some(kind) = super::for_loops::indices_receiver(selected) else {
        return Ok(None);
    };
    let Some((class, name)) = bound(kind) else {
        return Ok(None);
    };
    bodies
        .public_class_member_getter(
            &selected.identity().module,
            &KlibPublicIdSignature::classifier(class),
            name,
        )
        .map(Some)
        .ok_or_else(|| {
            KlibBodyDeclineReason::SignatureMismatch(
                "the receiver of an `indices` loop has no single bounding property".to_owned(),
            )
        })
}
