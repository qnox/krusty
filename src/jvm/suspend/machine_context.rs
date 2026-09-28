//! What every state machine of one file is built against, and the facts its builders publish to
//! emission.

use super::emission_facts::SuspendResultForwards;
use crate::types::Ty;
use std::cell::RefCell;

pub(super) struct MachineContext<'a> {
    /// Every function's declared (pre-CPS) return type.
    pub(super) orig_rets: &'a [Ty],
    /// Whether the stdlib declares the exact public static
    /// `SpillingKt.nullOutSpilledVariable(Object): Object` probe.
    pub(super) null_out_dead_spills: bool,
    /// The adaptation each continuation's `invokeSuspend` applies to the result it re-enters with.
    pub(super) forwards: RefCell<SuspendResultForwards>,
}

impl<'a> MachineContext<'a> {
    pub(super) fn new(orig_rets: &'a [Ty], null_out_dead_spills: bool) -> Self {
        Self {
            orig_rets,
            null_out_dead_spills,
            forwards: RefCell::default(),
        }
    }
}
