//! Semantic member-call edge facts shared by checking and common lowering.

/// What a selected member call inherits from its declaration's exact override family.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CheckedMemberCallEdge {
    pub(crate) semantic_role: Option<crate::types::SemanticCallRole>,
    pub(crate) overridden: Box<[crate::types::OverriddenDeclaration]>,
}

impl CheckedMemberCallEdge {
    /// Whether the call reaches one of `kotlin.Any`'s members (or a declaration overriding one),
    /// which every target realizes from the receiver's own representation.
    pub(crate) fn realizes_any_member(&self) -> bool {
        matches!(
            self.semantic_role,
            Some(
                crate::types::SemanticCallRole::KotlinAnyEquals
                    | crate::types::SemanticCallRole::KotlinAnyHashCode
                    | crate::types::SemanticCallRole::KotlinAnyToString
            )
        )
    }
}
