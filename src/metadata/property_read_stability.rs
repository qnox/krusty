//! Whether repeated reads of a property observe one value, as its Kotlin metadata declares it.
//!
//! JVM class files and KLIBs serialize the same property message, so one reading of its flag words
//! serves both. The answer is the declaration's own stability; how much of it another module may
//! rely on is [`crate::libraries::PropertyReadStability::across_modules`].

use super::property_flags::{
    ACCESSOR_IS_NOT_DEFAULT, DEFAULT, IS_DELEGATED, IS_VAR, MODALITY_MASK,
};
use crate::libraries::PropertyReadStability;

/// A property's declared read stability, from its modern flag word (`modern`), whether it carries
/// only the legacy word instead (`legacy_only`), and its getter's flag word. kotlinc omits the
/// modern word when it equals [`DEFAULT`] and the getter word when it equals the property's
/// default, so an absent getter word is a default getter. The legacy word lacks the getter facts,
/// so a property described only by it is unstable.
pub(crate) fn declared_read_stability(
    modern: Option<u64>,
    legacy_only: bool,
    getter: Option<u64>,
    has_context_parameters: bool,
) -> PropertyReadStability {
    let flags = match modern {
        Some(flags) => flags,
        None if legacy_only => return PropertyReadStability::Unstable,
        None => DEFAULT,
    };
    let custom_getter = getter.is_some_and(|getter| getter & ACCESSOR_IS_NOT_DEFAULT != 0);
    PropertyReadStability::from_declaration(
        flags & IS_VAR != 0,
        custom_getter || flags & IS_DELEGATED != 0,
        // Open, abstract, or sealed: an override may replace the default getter.
        flags & MODALITY_MASK != 0,
        has_context_parameters,
    )
}

#[cfg(test)]
mod tests {
    use super::declared_read_stability;
    use crate::libraries::PropertyReadStability;
    use crate::metadata::property_flags::{ACCESSOR_IS_NOT_DEFAULT, DEFAULT, IS_DELEGATED, IS_VAR};

    #[test]
    fn declaration_facts_define_property_read_stability() {
        let read = |modern, getter, contexts| {
            declared_read_stability(Some(modern), false, getter, contexts)
        };
        assert_eq!(read(0, None, false), PropertyReadStability::Stable);
        assert_eq!(
            read(1 << 4, None, false),
            PropertyReadStability::StableOnFinalReceiver
        );
        assert_eq!(read(IS_VAR, None, false), PropertyReadStability::Unstable);
        assert_eq!(
            read(IS_DELEGATED, None, false),
            PropertyReadStability::Unstable
        );
        assert_eq!(
            read(0, Some(ACCESSOR_IS_NOT_DEFAULT), false),
            PropertyReadStability::Unstable
        );
        assert_eq!(read(0, None, true), PropertyReadStability::Unstable);
    }

    #[test]
    fn an_omitted_flag_word_is_the_default_val_and_a_legacy_word_is_unstable() {
        assert_eq!(
            declared_read_stability(None, false, None, false),
            declared_read_stability(Some(DEFAULT), false, None, false)
        );
        assert_eq!(
            declared_read_stability(None, false, None, false),
            PropertyReadStability::Stable
        );
        assert_eq!(
            declared_read_stability(None, true, None, false),
            PropertyReadStability::Unstable
        );
    }
}
