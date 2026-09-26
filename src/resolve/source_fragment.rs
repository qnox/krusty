//! How much of a file the pass currently running actually holds.
//!
//! Most checking sees a COMPLETE file: every declaration, every expression. Restricted passes
//! deliberately retain only the syntax they own. Some re-enter declarations only to rebuild
//! lexical scopes, while metadata publication keeps just its selected header expressions. Syntax
//! belonging to neighboring declarations has already been released.
//!
//! The distinction has to be an explicit mode rather than a fact inferred from whichever data a
//! pass happens to carry. Ordinary Pass 2 also supplies selected declarations, so
//! "selection data is present" cannot separate a restricted pass from a complete one: inferring it
//! that way lets an invalid or incomplete fragment silently skip released syntax, which is precisely
//! the assertion's job to catch.

/// Which fragment of a file the running pass holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum SourceFragmentMode {
    /// Every declaration and every expression of the file. Fail-closed: released syntax must never
    /// be reachable, so encountering it is a defect in the pass that released it.
    #[default]
    Complete,
    /// Inline preparation: only the inline declarations that must be expanded here were selected.
    /// The annotation expressions of the members it skipped are outside the retained fragment.
    InlinePreparation,
    /// Pass-1 default checking: only signature defaults are checked, and the declaration is
    /// re-entered solely to recreate its scopes.
    SignatureDefaults,
}

impl SourceFragmentMode {
    /// Whether this pass may legitimately meet an annotation argument whose syntax is gone.
    ///
    /// Only a RESTRICTED pass may. A complete pass holds every expression, so released annotation
    /// syntax means something released syntax it still needs.
    pub(crate) fn may_observe_released_annotation_syntax(self) -> bool {
        matches!(self, Self::InlinePreparation | Self::SignatureDefaults)
    }

    /// Pass-1 default checking restricts far more than annotation syntax — expression depth,
    /// selected roots and the source-range authority all key on it.
    pub(crate) fn is_signature_defaults(self) -> bool {
        matches!(self, Self::SignatureDefaults)
    }
}

#[cfg(test)]
mod tests {
    use super::SourceFragmentMode;

    /// The whole point of the mode: a COMPLETE pass stays fail-closed, whatever data it carries.
    #[test]
    fn only_a_restricted_fragment_may_observe_released_annotation_syntax() {
        assert!(!SourceFragmentMode::Complete.may_observe_released_annotation_syntax());
        assert!(SourceFragmentMode::InlinePreparation.may_observe_released_annotation_syntax());
        assert!(SourceFragmentMode::SignatureDefaults.may_observe_released_annotation_syntax());
    }

    /// Only Pass-1 default checking carries the signature-defaults restrictions; inline preparation
    /// is restricted in a different way and must not inherit them.
    #[test]
    fn inline_preparation_is_not_signature_defaults() {
        assert!(SourceFragmentMode::SignatureDefaults.is_signature_defaults());
        assert!(!SourceFragmentMode::InlinePreparation.is_signature_defaults());
        assert!(!SourceFragmentMode::Complete.is_signature_defaults());
    }

    /// A pass that says nothing about itself is complete, so a new call site is fail-closed unless
    /// it deliberately declares otherwise.
    #[test]
    fn the_default_fragment_is_complete() {
        assert_eq!(SourceFragmentMode::default(), SourceFragmentMode::Complete);
    }
}
