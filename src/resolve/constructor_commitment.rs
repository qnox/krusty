//! Selected-constructor commitment and its origin-neutral declaration contract.

use crate::ast::ExprId;
use crate::libraries::{LibraryMember, LibraryType};
use crate::types::Ty;

use super::{Checker, ClassSig, ResolvedConstructor};

/// The declaration-owned generic shape shared by every selected constructor origin. Selection
/// normalizes source and provider declarations here; later publication consumes this record without
/// inspecting a constructor realization or querying a symbol provider again.
#[derive(Clone, Debug)]
pub(super) struct ConstructorDeclarationShape {
    pub(super) formals: Vec<String>,
    pub(super) bounds: Vec<Ty>,
    pub(super) parameters: Vec<Ty>,
}

impl ConstructorDeclarationShape {
    pub(super) fn source(class: &ClassSig, parameters: Vec<Ty>) -> Self {
        Self {
            formals: class.type_params().clone(),
            bounds: class.type_param_bounds().clone(),
            parameters,
        }
    }

    pub(super) fn provider(classifier: &LibraryType, member: &LibraryMember) -> Self {
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        Self {
            formals: classifier.type_params().clone(),
            bounds: classifier
                .type_param_bounds()
                .iter()
                .map(|bounds| bounds.first().copied().unwrap_or(any))
                .collect(),
            parameters: member.generic_sig.as_ref().map_or_else(
                || member.params.clone(),
                |signature| signature.params.clone(),
            ),
        }
    }
}

impl Checker<'_> {
    pub(super) fn discard_constructor_commitment(&mut self, call: ExprId) {
        self.resolved_constructors.remove(&call);
        self.discard_constructor_declaration_shape(call);
    }

    /// Commit the one semantic target and normalized declaration shape of a construction call.
    /// Rechecking during inference may have left an earlier callable probe for the same expression;
    /// that probe is not an alternative target and must not cross the checked boundary.
    pub(super) fn commit_constructor(
        &mut self,
        call: ExprId,
        target: ResolvedConstructor,
        declaration: ConstructorDeclarationShape,
    ) {
        self.resolved_calls.remove(&call);
        self.record_constructor_declaration_shape(call, declaration);
        self.resolved_constructors.insert(call, target);
    }
}
