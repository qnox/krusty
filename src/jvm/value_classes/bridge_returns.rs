//! What a bridge does with its RETURN value when the supertype spells a value class UNBOXED.
//!
//! The decision has to be made here, while the bridge's `erased_ret` still names the value class:
//! the pass rewrites it to the carrier in the same step, and nothing in the bridge names the class
//! afterwards. What comes out is a physical plan for emission, not a declaration fact.

use super::*;
use crate::ir::{Bridge, JvmBridgeReturnUnboxing};

/// The unboxing this bridge's return needs, or `None` when the supertype does not spell the value
/// class unboxed and the bridge's erased return therefore stays as it is. Where it is spelled
/// unboxed the bridge's descriptor returns the CARRIER while the override it delegates to still
/// hands back a reference — an erased generic `Object`, or the boxed class itself — so the carrier
/// comes out of the class's own `unbox-impl`.
///
/// `value_class` is the bridge's erased return read as a known value class — `None` for anything
/// else, including a carrier this module does not lower.
pub(super) fn plan_unboxing(
    bridge: &Bridge,
    value_class: Option<TypeName>,
    under: &std::collections::HashMap<TypeName, Ty>,
) -> Option<JvmBridgeReturnUnboxing> {
    let owner = value_class?;
    // A nullable `X?` whose underlying is itself null-carrying stays UNBOXED and carries the null;
    // one that BOXES (over a primitive, or a null-capable chain) is a reference the bridge returns
    // as it is.
    if bridge.erased_ret.is_nullable() && nullable_is_boxed(owner, under) {
        return None;
    }
    Some(JvmBridgeReturnUnboxing {
        owner,
        // `unbox-impl` is an instance call: a legally null result must go past it, not into it.
        null_preserving: bridge.erased_ret.is_nullable(),
    })
}
