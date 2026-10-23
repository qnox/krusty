//! Selection and terminal diagnostic ownership for bare calls through implicit receivers.
//!
//! Each receiver rung retains the normalized declarations it actually considered. If every later
//! rung declines, diagnostics consume that exact family; they never repeat provider lookup after
//! selection has failed.

use super::*;
use crate::types::TyKind;

pub(super) struct ReceiverCallRejections {
    receiver: Ty,
    candidates: Vec<crate::libraries::FunctionInfo>,
    mapping_failure: Option<MemberMappingFailure>,
}

impl ReceiverCallRejections {
    pub(super) fn has_mapping_failure(&self) -> bool {
        self.mapping_failure.is_some()
    }
}

impl Checker<'_> {
    pub(super) fn report_rejected_implicit_receiver_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        name: &str,
        expected: Option<Ty>,
        rejected: ReceiverCallRejections,
    ) {
        let type_args = self.resolved_explicit_type_args(scope, call_args.call);
        self.report_inapplicable_receiver_candidates(
            scope,
            call_args,
            name,
            CallConstraints {
                type_args: &type_args,
                receiver: rejected.receiver,
                expected,
            },
            &rejected.candidates,
        );
    }

    pub(super) fn this_member_call_ret(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        tower_rung: MemberCallTowerRung,
        name: &str,
        expected: Option<Ty>,
        retained: &mut Option<ReceiverCallRejections>,
    ) -> Option<Ty> {
        let receiver = tower_rung
            .receiver
            .expect("this-member selection requires an implicit-receiver rung");
        let CallArgs { call, args, .. } = call_args;
        let rt = self.apply_postponed_call_bindings(receiver.ty);
        let type_args = self.resolved_explicit_type_args(scope, call);
        // A platform type `T!` is usable as `T` for member calls, as on an explicit receiver.
        let member_receiver = match rt.kind() {
            TyKind::PlatformNullable(inner) => inner,
            _ => rt,
        };
        let (rung, rejected_candidates, mut member_mapping_failure) = if member_receiver
            == Ty::String
            || matches!(
                member_receiver.kind(),
                TyKind::Obj(..) | TyKind::TyParam(..)
            ) {
            match self.record_member_call_with_slots(
                scope,
                call,
                (rt, name),
                args,
                tower_rung,
                expected,
            ) {
                MemberSlotCall::Resolved(ret) => {
                    self.mark_implicit_receiver_selection(call, receiver);
                    return Some(ret);
                }
                MemberSlotCall::Ambiguous | MemberSlotCall::Rejected => {
                    return Some(Ty::Error);
                }
                MemberSlotCall::ExtensionRung {
                    extension,
                    member_mapping_failure: failure,
                    rejected_candidates: candidates,
                } => (extension, candidates, failure.map(|failure| *failure)),
            }
        } else {
            let callables = self.stable_receiver_callables(rt, name);
            let rejected = callables.functions().to_vec();
            (
                self.select_extension_rung(scope, call_args, rt, &type_args, expected, &callables),
                rejected,
                None,
            )
        };
        if rung.selection.is_none() {
            if member_mapping_failure.is_none() {
                member_mapping_failure = self
                    .unanimous_callable_mapping_failure(
                        scope,
                        call,
                        args,
                        &type_args,
                        &rung.overloads,
                    )
                    .map(|(failure, candidate)| MemberMappingFailure { failure, candidate });
            }
            if !rejected_candidates.is_empty() {
                retained.get_or_insert(ReceiverCallRejections {
                    receiver: rt,
                    candidates: rejected_candidates,
                    mapping_failure: member_mapping_failure,
                });
            }
            return None;
        }
        let ret = self.record_extension_selection(
            scope,
            call_args,
            name,
            CallConstraints {
                type_args: &type_args,
                receiver: rt,
                expected,
            },
            rung,
        );
        if let Some(ret) = ret {
            self.mark_implicit_receiver_selection(call, receiver);
            return Some(ret);
        }
        if !rejected_candidates.is_empty() {
            retained.get_or_insert(ReceiverCallRejections {
                receiver: rt,
                candidates: rejected_candidates,
                mapping_failure: member_mapping_failure,
            });
        }
        None
    }
}
