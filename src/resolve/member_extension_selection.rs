use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberExtensionSelection {
    All,
    Operators,
    DelegateConventions,
}

/// Declaration-owned facts retained when a member extension is not selected as a delegate
/// convention. These are captured before contextual instantiation: an unsatisfied context or an
/// inapplicable generated-accessor argument is itself one reason the declaration must still appear
/// in the diagnostic.
#[derive(Clone)]
pub(crate) struct MemberExtensionConventionDiagnosticCandidate {
    pub(crate) stable_declaration: Option<crate::fir::DeclarationId>,
    pub(crate) extension_receiver: Ty,
    pub(crate) params: Vec<Ty>,
    pub(crate) context_count: usize,
    pub(crate) ret: Ty,
    pub(crate) parameter_names: Vec<String>,
}

impl MemberExtensionConventionDiagnosticCandidate {
    pub(super) fn from_shape(shape: &MemberExtensionFunctionShape) -> Self {
        let signature = &shape.function.signature;
        let declared_ret = signature
            .generic_sig
            .as_ref()
            .map_or(signature.ret, |generic| generic.ret);
        Self {
            stable_declaration: signature.stable_declaration,
            extension_receiver: shape.priority.declared_receiver,
            params: shape.declared_params.clone(),
            context_count: signature.context_count,
            ret: apply_inference_bindings(declared_ret, &shape.class_bindings),
            parameter_names: signature.call_sig().param_names,
        }
    }
}

pub(crate) enum MemberExtensionFunctionSelection {
    /// Boxed: the candidate is ~950 bytes while the diagnostic variants carry only a `Vec`.
    Selected(Box<MemberExtensionFunctionCandidate>),
    None(Vec<MemberExtensionConventionDiagnosticCandidate>),
    Ambiguous(Vec<MemberExtensionFunctionCandidate>),
}

pub(super) fn exclude_delegate_convention(
    shape: &MemberExtensionFunctionShape,
    selection: MemberExtensionSelection,
    excluded: &mut Vec<MemberExtensionConventionDiagnosticCandidate>,
) -> bool {
    let exclude = selection == MemberExtensionSelection::DelegateConventions
        && !delegated_properties::is_usable_delegate_convention(
            shape.is_operator,
            shape.function.signature.context_count,
        );
    if exclude {
        excluded.push(MemberExtensionConventionDiagnosticCandidate::from_shape(
            shape,
        ));
    }
    exclude
}

pub(super) fn retain_inapplicable_delegate_convention(
    shape: &MemberExtensionFunctionShape,
    selection: MemberExtensionSelection,
    excluded: &mut Vec<MemberExtensionConventionDiagnosticCandidate>,
) {
    if selection == MemberExtensionSelection::DelegateConventions {
        excluded.push(MemberExtensionConventionDiagnosticCandidate::from_shape(
            shape,
        ));
    }
}

pub(super) fn retain_selected(
    candidates: &mut Vec<MemberExtensionFunctionCandidate>,
    selection: MemberExtensionSelection,
) {
    if selection == MemberExtensionSelection::Operators {
        candidates.retain(|candidate| candidate.is_operator);
    }
}

/// Member-extension dispatch prefers an ordinary implicit receiver over a context-parameter
/// receiver. Both groups keep nearest-first order, and a context receiver stays eligible when no
/// ordinary receiver declares the member.
pub(super) fn ordinary_dispatch_first(
    scope: &super::CheckerScope<'_>,
    mut receivers: Vec<ImplicitReceiver>,
) -> Vec<ImplicitReceiver> {
    receivers.sort_by_key(|receiver| {
        u8::from(scope.implicit_receiver_context(receiver.identity).is_some())
    });
    receivers
}

pub(super) fn maximal_member_extensions<T>(
    oracle: &dyn crate::assignable::TypeOracle,
    candidates: &[T],
    priority: impl Fn(&T) -> MemberExtensionPriority,
) -> Vec<usize> {
    let Some(nearest_dispatch) = candidates
        .iter()
        .map(|candidate| priority(candidate).dispatch_rank)
        .min()
    else {
        return Vec::new();
    };
    candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| priority(candidate).dispatch_rank == nearest_dispatch)
        .filter_map(|(index, candidate)| {
            let candidate = priority(candidate);
            let dominated = candidates.iter().enumerate().any(|(other_index, other)| {
                if index == other_index {
                    return false;
                }
                let other = priority(other);
                if other.dispatch_rank != nearest_dispatch {
                    return false;
                }
                // Among receivers applicable to this same call-site type, a concrete receiver
                // is more specific than one whose shape had to infer a method parameter. This
                // also covers invariant nested shapes (`Box<String>` versus `Box<T>`): comparing
                // their instantiated `Box<String>`/`Box<Any>` views as ordinary subtypes loses
                // the declaration-level genericity that Kotlin's specificity rule uses.
                if candidate.generic_receiver != other.generic_receiver {
                    return candidate.generic_receiver && !other.generic_receiver;
                }
                let other_is_subtype = crate::assignable::is_assignable(
                    &crate::assignable::TyCtx::new(),
                    oracle,
                    other.receiver_domain,
                    candidate.receiver_domain,
                );
                let candidate_is_subtype = crate::assignable::is_assignable(
                    &crate::assignable::TyCtx::new(),
                    oracle,
                    candidate.receiver_domain,
                    other.receiver_domain,
                );
                if candidate.receiver_domain == other.receiver_domain
                    || (other_is_subtype && candidate_is_subtype)
                {
                    let other_owner_is_subtype = crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        oracle,
                        Ty::obj_name(other.owner),
                        Ty::obj_name(candidate.owner),
                    );
                    let candidate_owner_is_subtype = crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        oracle,
                        Ty::obj_name(candidate.owner),
                        Ty::obj_name(other.owner),
                    );
                    if other_owner_is_subtype != candidate_owner_is_subtype {
                        return other_owner_is_subtype;
                    }
                    return other.dispatch_depth < candidate.dispatch_depth;
                }
                other_is_subtype && !candidate_is_subtype
            });
            (!dominated).then_some(index)
        })
        .collect()
}

pub(super) fn candidate(
    shape: &MemberExtensionFunctionShape,
    instantiated: InstantiatedMemberExtension,
) -> MemberExtensionFunctionCandidate {
    MemberExtensionFunctionCandidate {
        stable_declaration: shape.function.signature.stable_declaration,
        external_identity: shape.function.external_identity,
        external_default_provider: shape.function.external_default_provider,
        priority: shape.priority,
        dispatch_receiver: shape.dispatch_receiver,
        score: instantiated.score,
        physical_receiver: shape.function.physical_receiver,
        extension_receiver: instantiated.extension_receiver,
        params: instantiated.logical_params,
        visible_params: instantiated.visible_params,
        physical_params: shape.function.physical_params.clone(),
        context_args: instantiated.context_sources,
        context_count: shape.function.signature.context_count,
        ret: instantiated.ret,
        physical_ret: shape.function.signature.ret,
        call_sig: instantiated.call_sig,
        diagnostic_param_names: shape.function.signature.call_sig().param_names,
        physical_vararg_index: instantiated.physical_vararg_index,
        argument_parameters: instantiated.argument_parameters,
        visibility: shape.function.signature.visibility,
        is_operator: shape.is_operator,
        inline: InlineKind::from_flags(
            shape.function.signature.is_inline(),
            shape.function.signature.requires_splice(),
        ),
        inline_body_plan: shape.function.inline_body_plan.clone(),
        suspend: shape.function.signature.is_suspend(),
        declared_params: shape
            .function
            .signature
            .generic_sig
            .as_ref()
            .map(|signature| signature.params.clone())
            .unwrap_or_else(|| shape.function.signature.params.clone()),
        declared_ret: shape.function.declared_ret,
        owner: shape.owner,
        physical_name: shape.function.physical_name.clone(),
    }
}

impl MemberExtensionFunctionSelection {
    pub(super) fn into_checker_result(
        self,
    ) -> Result<Option<MemberExtensionFunctionCandidate>, ()> {
        match self {
            Self::Selected(selected) => Ok(Some(*selected)),
            Self::None(_) => Ok(None),
            Self::Ambiguous(_) => Err(()),
        }
    }
}

impl MemberExtensionFunctionCandidate {
    pub(super) fn resolved_call(
        &self,
        dispatch_receiver: ImplicitReceiverSelection,
        extension_receiver: Ty,
        interface: bool,
    ) -> ResolvedCall {
        ResolvedCall::MemberExtension {
            stable_declaration: self.stable_declaration,
            external_identity: self.external_identity,
            external_default_provider: self.external_default_provider,
            owner: self.owner,
            dispatch_receiver,
            extension_receiver,
            physical_receiver: self.physical_receiver,
            // Selection and diagnostics used the Kotlin source name. Only the finalized emit target
            // receives this provider-owned spelling, so a mangled dependency method cannot leak back
            // into a diagnostic or become a parallel lookup key.
            name: self.physical_name.clone(),
            params: self.params.clone(),
            physical_params: self.physical_params.clone(),
            context_args: self.context_args.clone(),
            ret: self.ret,
            physical_ret: self.physical_ret,
            inline: self.inline,
            inline_body_plan: self.inline_body_plan.clone(),
            suspend: self.suspend,
            declared_ret: self.declared_ret,
            interface,
            vararg_index: self.physical_vararg_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ordinary_dispatch_first, ImplicitReceiver, MemberExtensionFunctionSelection};
    use crate::resolve::scope::{ContextReceiver, ContextReceiverKind, ScopeKind};
    use crate::types::Ty;

    #[test]
    fn a_selection_carries_a_pointer_to_its_callable() {
        assert_eq!(std::mem::size_of::<MemberExtensionFunctionSelection>(), 32);
    }

    #[test]
    fn ordinary_dispatch_receivers_keep_their_relative_priority_before_context_receivers() {
        let root: super::super::CheckerScope<'_> = super::super::CheckerScope::root();
        let outer = root.child(ScopeKind::Class {
            ty: Ty::obj("test/Outer"),
            carries_outer: true,
        });
        let inner = outer.child(ScopeKind::Class {
            ty: Ty::obj("test/Inner"),
            carries_outer: true,
        });
        let scope = inner.function_child(
            Some(Ty::obj("test/Extension")),
            None,
            &[
                ContextReceiver::new(
                    Ty::obj("test/FirstContext"),
                    ContextReceiverKind::LegacyReceiver,
                    None,
                ),
                ContextReceiver::new(
                    Ty::obj("test/SecondContext"),
                    ContextReceiverKind::LegacyReceiver,
                    None,
                ),
            ],
        );
        let receivers = scope
            .implicit_receivers_with_declarations()
            .into_iter()
            .enumerate()
            .map(
                |(receiver_depth, (ty, extension_receiver, identity, class_receiver))| {
                    ImplicitReceiver {
                        ty,
                        declared_ty: ty,
                        identity,
                        extension_receiver,
                        class_receiver,
                        current: receiver_depth == 0,
                        receiver_depth,
                    }
                },
            )
            .collect::<Vec<_>>();

        let ordered = ordinary_dispatch_first(&scope, receivers);
        assert_eq!(
            ordered
                .into_iter()
                .map(|receiver| receiver.ty)
                .collect::<Vec<_>>(),
            vec![
                Ty::obj("test/Extension"),
                Ty::obj("test/Inner"),
                Ty::obj("test/Outer"),
                Ty::obj("test/SecondContext"),
                Ty::obj("test/FirstContext"),
            ]
        );
    }
}
