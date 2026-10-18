//! Provider normalization of Kotlin inline declarations.

use crate::libraries::InlineKind;

/// Normalize one metadata-declared function before it enters the common semantic model. Reified
/// type parameters make a direct erased call illegal even when the JVM method itself is public.
pub(super) fn metadata_inline(
    is_inline: bool,
    has_reified_type_parameters: bool,
    bytecode_public: bool,
) -> InlineKind {
    InlineKind::from_flags(
        is_inline,
        is_inline && (has_reified_type_parameters || !bytecode_public),
    )
}

/// A property accessor's inline-ness: its metadata `inline` flag, and a semantically visible Kotlin
/// property whose exact classfile accessor is non-public is an inline body container, not a
/// callable fallback.
pub(super) fn property_accessor_inline(declared_inline: bool, bytecode_public: bool) -> InlineKind {
    InlineKind::from_flags(declared_inline || !bytecode_public, !bytecode_public)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_reification_and_visibility_define_the_splice_obligation() {
        assert_eq!(metadata_inline(false, false, true), InlineKind::None);
        assert_eq!(metadata_inline(true, false, true), InlineKind::CanInline);
        assert_eq!(metadata_inline(true, false, false), InlineKind::MustInline);
        assert_eq!(metadata_inline(true, true, true), InlineKind::MustInline);
        assert_eq!(metadata_inline(true, true, false), InlineKind::MustInline);
    }

    #[test]
    fn reified_flag_without_inline_does_not_invent_an_inline_declaration() {
        assert_eq!(metadata_inline(false, true, true), InlineKind::None);
    }

    #[test]
    fn an_accessor_is_inline_by_its_metadata_flag_or_its_hidden_method() {
        assert_eq!(property_accessor_inline(false, true), InlineKind::None);
        assert_eq!(property_accessor_inline(true, true), InlineKind::CanInline);
        assert_eq!(
            property_accessor_inline(false, false),
            InlineKind::MustInline
        );
        assert_eq!(
            property_accessor_inline(true, false),
            InlineKind::MustInline
        );
    }
}
