//! Checked call target shapes shared by call selection and FIR publication.

use crate::fir::{ExternalCallableId, FirCallTarget, FirTypeSubstitution, ResolvedTy};
use crate::types::Ty;

pub(in crate::fir::body_check) struct MemberExtensionFirTarget {
    pub(in crate::fir::body_check) target: FirCallTarget,
    pub(in crate::fir::body_check) substitutions: Box<[FirTypeSubstitution]>,
    /// Source-visible semantic parameters. The external target separately inserts its extension
    /// receiver into the provider parameter list.
    pub(in crate::fir::body_check) parameters: Vec<Ty>,
    pub(in crate::fir::body_check) extension_parameter: Option<u32>,
    /// The extension receiver parameter's type as the checker applied it, which an explicit
    /// receiver argument is converted to.
    pub(in crate::fir::body_check) extension_receiver: Ty,
}

pub(super) struct ExternalCallTarget<'a> {
    pub(super) declaration: ExternalCallableId,
    pub(super) receiver: Option<Ty>,
    pub(super) declared_receiver: Option<Ty>,
    pub(super) parameters: Vec<Ty>,
    pub(super) result: Ty,
    pub(super) declared_result: Option<Ty>,
    pub(super) overridden_results: &'a [Ty],
    pub(super) semantic_role: Option<crate::types::SemanticCallRole>,
    pub(super) overridden_declarations: &'a [crate::types::OverriddenDeclaration],
    pub(super) suspend: bool,
    pub(super) can_inline: bool,
    pub(super) inline_plan: Option<&'a crate::libraries::InlineBodyPlan>,
    pub(super) inline_receiver_parameter: Option<usize>,
    /// Unsubstituted type-parameter receiver the inline plan stores as its erased bound.
    pub(super) inline_declared_receiver: Option<Ty>,
    pub(super) reified_type_parameter_ordinals: &'a [u32],
}

/// One already-selected operator target. Context parameters stay separate while source operands
/// are mapped, then join the value parameters in the checked call's semantic parameter list.
pub(in crate::fir::body_check) struct SelectedOperatorTarget {
    pub(in crate::fir::body_check) target: FirCallTarget,
    pub(in crate::fir::body_check) extension: bool,
    pub(in crate::fir::body_check) context_parameters: Box<[ResolvedTy]>,
    pub(in crate::fir::body_check) value_parameters: Box<[ResolvedTy]>,
    pub(in crate::fir::body_check) vararg_index: Option<usize>,
    pub(in crate::fir::body_check) context_arguments:
        Vec<Option<crate::resolve::ResolvedContextArgument>>,
}

impl SelectedOperatorTarget {
    pub(in crate::fir::body_check) fn parameter_types(&self) -> Box<[ResolvedTy]> {
        self.context_parameters
            .iter()
            .chain(self.value_parameters.iter())
            .copied()
            .collect()
    }
}
