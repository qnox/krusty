//! Semantic member-call edge facts shared by checking and common lowering.

/// What a selected member call inherits from its declaration's exact override family.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CheckedMemberCallEdge {
    pub(crate) semantic_role: Option<crate::types::SemanticCallRole>,
    pub(crate) overridden: Box<[crate::types::OverriddenDeclaration]>,
}
