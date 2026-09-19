use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemberExtensionSelection {
    All,
    Operators,
    DelegateConventions,
}

/// Declaration-owned facts retained when a member extension is deliberately excluded from
/// delegate-convention selection. These are captured before contextual instantiation: an
/// unsatisfied context is itself one reason the declaration must still appear in the diagnostic.
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
    Selected(MemberExtensionFunctionCandidate),
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

pub(super) fn retain_selected(
    candidates: &mut Vec<MemberExtensionFunctionCandidate>,
    selection: MemberExtensionSelection,
) {
    if selection == MemberExtensionSelection::Operators {
        candidates.retain(|candidate| candidate.is_operator);
    }
}

impl MemberExtensionFunctionSelection {
    pub(super) fn into_checker_result(
        self,
    ) -> Result<Option<MemberExtensionFunctionCandidate>, ()> {
        match self {
            Self::Selected(selected) => Ok(Some(selected)),
            Self::None(_) => Ok(None),
            Self::Ambiguous(_) => Err(()),
        }
    }
}
