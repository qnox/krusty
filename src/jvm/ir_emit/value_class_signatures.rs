//! The generic `Signature` positions of a function the JVM value-class pass realized as a static
//! replacement over its class's carrier.
//!
//! kotlinc moves the dispatch receiver into the first parameter, typed as the class itself (`C<T>`),
//! and its signature writer expands that position like any other value-class position
//! (`signature_formatter::value_class_positions`). The function's declared positions keep their
//! semantic types, so a value class among them is expanded the same way. The IR shape itself stays
//! semantic, since the declaration's metadata is written from it.

use std::borrow::Cow;

use crate::ir::{IrFile, IrGenericSig};
use crate::types::Ty;

pub(super) fn physical_generic_signature<'a>(
    ir: &IrFile,
    fid: u32,
    generic: &'a IrGenericSig,
) -> Cow<'a, IrGenericSig> {
    let Some(carrier) = moved_receiver(ir, fid) else {
        return Cow::Borrowed(generic);
    };
    let mut physical = generic.clone();
    physical.params.insert(0, carrier);
    Cow::Owned(physical)
}

/// A member's recorded semantic parameters and result (an accessor's, or one using its class's
/// type parameters), after the receiver a static replacement takes first.
pub(super) fn physical_member_signature<'a>(
    ir: &'a IrFile,
    fid: u32,
) -> Option<(Cow<'a, [Ty]>, Ty)> {
    let (params, ret) = ir.member_semantic_sigs.get(&fid)?;
    Some(match moved_receiver(ir, fid) {
        Some(carrier) => (
            Cow::Owned(
                std::iter::once(carrier)
                    .chain(params.iter().copied())
                    .collect(),
            ),
            *ret,
        ),
        None => (Cow::Borrowed(params.as_slice()), *ret),
    })
}

/// The declared parameters and result of a function whose value-class positions the JVM pass
/// erased, after the receiver a static replacement takes first.
pub(super) fn declared_value_class_signature(ir: &IrFile, fid: u32) -> Option<(Vec<Ty>, Ty)> {
    let (_, params, ret) = ir.vc_declared_sigs.get(&fid)?;
    let params = moved_receiver(ir, fid)
        .into_iter()
        .chain(params.iter().copied())
        .collect();
    Some((params, *ret))
}

/// The class type `C<T>` a static replacement's moved receiver is declared with.
fn moved_receiver(ir: &IrFile, fid: u32) -> Option<Ty> {
    if !ir.jvm_value_class_receiver_impls.contains(&fid) {
        return None;
    }
    let class = ir
        .classes
        .iter()
        .find(|class| class.methods.contains(&fid))
        .expect("a value-class static replacement is a member of its class");
    Some(ir.class_type(class))
}
