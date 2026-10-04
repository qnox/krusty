//! The functions a value class declares as its own members, apart from the body-local helpers it
//! only hosts.
//!
//! A lambda, local function, or callable-reference adapter written inside a value-class member is
//! lifted to a private static function of that value class, but it is not a member: it has no
//! dispatch receiver, a captured `this` reaches it as an ordinary carrier parameter, and kotlinc
//! lowers its body like that of any other static function. Every representation boundary in it
//! (an `Any` argument, local, or return; a captured value class handed to a reference slot) is
//! therefore coerced exactly as it is in a lambda lifted into an ordinary class.

use crate::ir::IrFile;
use std::collections::HashSet;

/// Every function a value class declares as a member: its source members (later realized as
/// static carrier functions) and its synthesized members. The body-local static helpers the
/// frontend placed in the class ([`IrFile::class_static_local_functions`]) are excluded by that
/// exact identity.
pub(super) fn value_class_members(ir: &IrFile) -> HashSet<u32> {
    ir.classes
        .iter()
        .filter(|class| class.is_value)
        .flat_map(|class| class.methods.iter().copied())
        .filter(|function| !ir.class_static_local_functions.contains_key(function))
        .collect()
}
