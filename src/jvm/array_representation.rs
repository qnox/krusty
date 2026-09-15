//! How a Kotlin primitive array is REPRESENTED on the JVM: the element width, and the opcodes and
//! `newarray` atype that follow from it.
//!
//! This is one question — "how wide is an element of this array, and which instruction moves it" —
//! and it used to be answered by four tables that each listed the array element types by hand. Each
//! had been extended with the unsigned names separately, and none of them completely: `UByte` and
//! `UShort` fell through every one, so a `UByteArray` was allocated as `int[]` and its elements
//! stored with `aastore`, which the verifier rejects. The tables here carry SIGNED arms only and
//! route through [`scalar_element`], so a thirteenth width cannot be added to some of them and not
//! others.

use crate::types::Ty;

/// The primitive an element of a PRIMITIVE array is carried as, which is what decides its width.
///
/// `Ty::scalar_value_repr` owns the rule, unsigned erasure included: a `UByteArray`'s element is a
/// `UByte` and is carried as a `Byte`. Reaching here with anything else means a reference array was
/// routed to a primitive opcode, which is an invariant failure rather than a shape to encode — the
/// silent `aaload`/`aastore`/`int` fallbacks these replaced are exactly how `UByte` and `UShort`
/// came to be stored as references without anything saying so.
fn scalar_element(elem: Ty, what: &str) -> Ty {
    elem.scalar_value_repr().unwrap_or_else(|| {
        unreachable!("a primitive array element that is not a scalar: {elem:?} ({what})")
    })
}

/// `(opcode, value-words)` for an array element load (`Xaload`).
pub(super) fn array_load_op(elem: Ty, reference_array: bool) -> (u8, i32) {
    if reference_array {
        return (0x32, 1);
    }
    // An unsigned array IS the signed array it is an inline class over, so the element's width —
    // and therefore the opcode — is the one `scalar_value_repr` already assigns it.
    match scalar_element(elem, "load") {
        Ty::Int => (0x2e, 1),
        Ty::Long => (0x2f, 2),
        Ty::Float => (0x30, 1),
        Ty::Double => (0x31, 2),
        Ty::Boolean | Ty::Byte => (0x33, 1),
        Ty::Char => (0x34, 1),
        Ty::Short => (0x35, 1),
        other => unreachable!("a primitive array element carried as {other:?} (load)"),
    }
}

/// `(opcode, value-words)` for an array element store (`Xastore`).
pub(super) fn array_store_op(elem: Ty, reference_array: bool) -> (u8, i32) {
    if reference_array {
        return (0x53, 1);
    }
    match scalar_element(elem, "store") {
        Ty::Int => (0x4f, 1),
        Ty::Long => (0x50, 2),
        Ty::Float => (0x51, 1),
        Ty::Double => (0x52, 2),
        Ty::Boolean | Ty::Byte => (0x54, 1),
        Ty::Char => (0x55, 1),
        Ty::Short => (0x56, 1),
        other => unreachable!("a primitive array element carried as {other:?} (store)"),
    }
}

/// `newarray` atype for a primitive element (JVMS Table 6.5.newarray-A).
pub(super) fn prim_newarray_atype(elem: Ty) -> u8 {
    match scalar_element(elem, "newarray") {
        Ty::Boolean => 4,
        Ty::Char => 5,
        Ty::Float => 6,
        Ty::Double => 7,
        Ty::Byte => 8,
        Ty::Short => 9,
        Ty::Int => 10,
        Ty::Long => 11,
        other => unreachable!("a primitive array element carried as {other:?} (newarray)"),
    }
}

/// The JVM array type a primitive specialized array CLASS is carried as (`kotlin/UIntArray` → `[I`),
/// or `None` for a name that is not one.
///
/// An unsigned array is an inline class over the signed primitive array and at the JVM level IS that
/// array, which is the same erasure a bare unsigned scalar gets — so the element comes from
/// [`scalar_element`] rather than from a second list of names. The list this replaced named
/// `UIntArray` and `ULongArray` only, so a `UByteArray` reached emission carried as a reference
/// array.
pub(super) fn prim_array_carrier(internal: impl crate::types::InternalName) -> Option<Ty> {
    let element = crate::types::prim_array_element(internal)?;
    Some(Ty::array(scalar_element(element, "carrier")))
}
