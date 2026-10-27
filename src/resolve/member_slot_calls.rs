//! Ordinary member and receiver-bound constructor selection at one scope-tower rung.

use super::*;

impl Checker<'_> {
    pub(super) fn record_member_call_with_slots(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        (rt, name): (Ty, &str),
        args: &[ExprId],
        tower_rung: MemberCallTowerRung,
        expected: Option<Ty>,
    ) -> MemberSlotCall {
        let type_arguments = self.resolved_explicit_type_args(scope, call);
        let arg_tys = args
            .iter()
            .map(|argument| self.expr_types[argument.0 as usize])
            .collect::<Vec<_>>();
        let callables = self.stable_receiver_callables(rt, name);
        crate::trace_compiler!(
            "fir",
            "implicit/member call rung call={call:?} receiver={rt:?} name={name} candidates={:?}",
            callables
                .functions()
                .iter()
                .map(|candidate| (
                    candidate.kind,
                    candidate.semantic_receiver(),
                    candidate.callable.owner,
                    candidate.callable.name.as_str(),
                ))
                .collect::<Vec<_>>(),
        );
        let (member_receiver, mut member_overloads, _) =
            self.body_local_member_overload_rung(rt, name);
        if let Some(receiver) = tower_rung
            .receiver
            .filter(|receiver| receiver.declared_ty != rt)
        {
            for candidate in self.private_members_hidden_by_smart_cast(receiver.declared_ty, name) {
                let already_present = member_overloads.iter().any(|existing| {
                    existing.callable.owner == candidate.callable.owner
                        && existing.callable.name == candidate.callable.name
                        && existing.callable.descriptor == candidate.callable.descriptor
                });
                if !already_present {
                    member_overloads.push(candidate);
                }
            }
        }
        let mut extension_overloads = callables
            .functions()
            .iter()
            .filter(|candidate| candidate.kind == crate::libraries::FnKind::Extension)
            .cloned()
            .collect::<Vec<_>>();
        for receiver in self.implicit_receivers(scope) {
            let Some(owner) =
                crate::symbol_resolver::member_scope_receiver(receiver.ty).obj_internal()
            else {
                continue;
            };
            let Some(local) = self
                .checked_local_methods
                .get(&owner)
                .and_then(|methods| methods.get(name))
            else {
                continue;
            };
            extension_overloads.extend(
                local
                    .iter()
                    .filter(|candidate| candidate.kind == crate::libraries::FnKind::Extension)
                    .map(|candidate| {
                        let mut candidate = candidate.clone();
                        candidate.scope_rank = u32::try_from(receiver.receiver_depth)
                            .expect("implicit receiver depth exceeds callable rank");
                        candidate
                    }),
            );
        }
        // Kotlin's member level holds the receiver's member functions together with the
        // constructors of the inner classifier it exposes under the same name. With member
        // functions present both families are one candidate list, selected once below. Without
        // them an explicit receiver selects over the constructors alone, still ahead of every
        // extension level; a bare call's constructor-only family belongs to its classifier tower.
        let file = self.file;
        let construction = BoundInnerConstruction {
            call_args: CallArgs {
                call,
                args,
                arg_tys: &arg_tys,
            },
            explicit_outer: tower_rung.explicit_receiver,
            receiver: rt,
            arg_names: file.call_arg_names.get(&call.0).map(Vec::as_slice),
            expected,
        };
        let mut inner_classifier = None;
        let mut inner_constructors = Vec::new();
        if !member_overloads.is_empty() {
            match self.member_level_inner_classifier(rt, name) {
                InheritedNestedClassifier::NotFound => {}
                InheritedNestedClassifier::Ambiguous => {
                    self.report_ambiguous_inner_classifier(call, name);
                    return MemberSlotCall::Rejected;
                }
                InheritedNestedClassifier::Found(internal) => {
                    inner_classifier = Some((internal, None));
                    inner_constructors = self.bound_inner_constructor_candidates(rt, internal);
                }
            }
        } else if tower_rung.explicit_receiver.is_some()
            && !extension_overloads
                .iter()
                .any(|candidate| candidate.constructor_alias.is_some())
        {
            match self.bound_inner_constructor_call(scope, construction, name) {
                Some(Ty::Error) => return MemberSlotCall::Rejected,
                Some(ret) => return MemberSlotCall::Resolved(ret),
                None => {}
            }
        }
        // kotlinc lists a member level's constructors ahead of its functions.
        let overloads = inner_constructors
            .iter()
            .chain(&member_overloads)
            .chain(&extension_overloads)
            .cloned()
            .collect::<Vec<_>>();
        let rejected_candidates = overloads.clone();
        let extension_offset = inner_constructors.len() + member_overloads.len();

        let selection = self.select_callable_candidate(
            scope,
            CallArgs {
                call,
                args,
                arg_tys: &arg_tys,
            },
            &type_arguments,
            Some(rt),
            CallResultConstraint::direct(expected),
            overloads,
        );
        let selected = match selection {
            Some(CallableCandidateSelection::Selected(selected)) => {
                if let Some(alias) = selected.info.constructor_alias() {
                    let target = crate::symbol_resolver::ty_subst_keep_unbound(
                        alias.expansion,
                        &selected.bindings,
                    );
                    let internal = selected
                        .info
                        .bound_inner_constructor
                        .expect("an alias constructor retains its target declaration")
                        .inner;
                    return self.commit_selected_inner_constructor(
                        scope,
                        construction,
                        (internal, Some(target)),
                        &selected.info,
                    );
                }
                if selected.is_extension() {
                    return MemberSlotCall::ExtensionRung {
                        extension: ExtensionRungSelection {
                            selection: Some(CallableCandidateSelection::Selected(selected)),
                            overloads: extension_overloads,
                        },
                        member_mapping_failure: None,
                        rejected_candidates,
                    };
                }
                if let Some((internal, alias_target)) =
                    inner_classifier.filter(|_| selected.info.bound_inner_constructor.is_some())
                {
                    return self.commit_selected_inner_constructor(
                        scope,
                        construction,
                        (internal, alias_target),
                        &selected.info,
                    );
                }
                selected.info.clone()
            }
            Some(CallableCandidateSelection::MissingContext(selected))
                if selected.is_extension() =>
            {
                return MemberSlotCall::ExtensionRung {
                    extension: ExtensionRungSelection {
                        selection: Some(CallableCandidateSelection::MissingContext(selected)),
                        overloads: extension_overloads,
                    },
                    member_mapping_failure: None,
                    rejected_candidates,
                };
            }
            Some(CallableCandidateSelection::MissingContext(_)) if tower_rung.is_implicit() => {
                // Context availability is part of applicability at each implicit-receiver
                // scope-tower rung. A nearer receiver whose same-named members all require an
                // unavailable context must not claim the call: extensions at this rung and then
                // enclosing receivers still get their ordinary opportunity. The terminal module
                // member probe retains the missing-context diagnostic when no later rung applies.
                return MemberSlotCall::ExtensionRung {
                    extension: ExtensionRungSelection {
                        selection: None,
                        overloads: extension_overloads,
                    },
                    member_mapping_failure: None,
                    rejected_candidates,
                };
            }
            Some(CallableCandidateSelection::MissingContext(selected)) => {
                self.report_selected_callable_missing_context(scope, call, &selected);
                return MemberSlotCall::Rejected;
            }
            Some(CallableCandidateSelection::Ambiguous(candidates, positions)) => {
                if candidates.iter().all(|candidate| candidate.is_extension()) {
                    return MemberSlotCall::ExtensionRung {
                        extension: ExtensionRungSelection {
                            // The positions move from the whole member level to its
                            // extension rung.
                            selection: Some(CallableCandidateSelection::Ambiguous(
                                candidates,
                                positions
                                    .into_iter()
                                    .map(|position| position - extension_offset)
                                    .collect(),
                            )),
                            overloads: extension_overloads,
                        },
                        member_mapping_failure: None,
                        rejected_candidates,
                    };
                }
                self.report_callable_ambiguity(call, name, &candidates);
                return MemberSlotCall::Ambiguous;
            }
            None => {
                let mapping_failure = self
                    .unanimous_callable_mapping_failure(
                        scope,
                        call,
                        args,
                        &type_arguments,
                        &member_overloads,
                    )
                    .map(|(failure, candidate)| {
                        Box::new(MemberMappingFailure { failure, candidate })
                    });
                return MemberSlotCall::ExtensionRung {
                    extension: ExtensionRungSelection {
                        selection: None,
                        overloads: extension_overloads,
                    },
                    member_mapping_failure: mapping_failure,
                    rejected_candidates,
                };
            }
        };

        let selected_receiver = self
            // A type parameter's lookup receiver is its complete declared bound intersection;
            // `rt` remains the source value type so the ordinary no-projection case still records
            // that value unchanged.
            .selected_intersection_member_receiver(member_receiver, &selected)
            .unwrap_or(rt);
        let mut member = self
            .resolver()
            .commit_selected_member_function(member_receiver, selected);
        // Candidate lookup may have reached an inherited body-local rung (`E.foo` for a receiver
        // of type `F`). That rung owns generic specialization and declaration selection, but the
        // checked call still dispatches on the source receiver `F`. In particular, Kotlin protected
        // access permits an inherited call through the subclass receiver and rejects the same call
        // through an arbitrary base-typed value.
        member.receiver = selected_receiver;
        match self.record_selected_member_call(scope, call, args, member) {
            Some(Ty::Error) => MemberSlotCall::Rejected,
            Some(ret) => MemberSlotCall::Resolved(ret),
            None => {
                self.diags.error(
                    self.call_callee_name_span(call),
                    "selected member call has an invalid context-argument shape".to_string(),
                );
                MemberSlotCall::Rejected
            }
        }
    }
}
