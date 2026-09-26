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
    use super::MemberExtensionFunctionSelection;

    #[test]
    fn a_selection_carries_a_pointer_to_its_callable() {
        assert_eq!(std::mem::size_of::<MemberExtensionFunctionSelection>(), 32);
    }
}
