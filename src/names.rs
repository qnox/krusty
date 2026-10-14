//! Kotlin source/metadata naming helpers shared across compiler phases.

use unicode_general_category::{get_general_category, GeneralCategory};

/// Kotlin's conventional name for an unnamed companion object, used for both the nested classifier
/// segment (`Outer$Companion`) and the singleton field (`Outer.Companion`).
pub(crate) const COMPANION_OBJECT_NAME: &str = "Companion";

/// Whether `character` may start an ordinary (non-backticked) Kotlin identifier.
///
/// This common name boundary is shared by tokenization and diagnostic source rendering, so both
/// use Kotlin's exact Unicode-category contract rather than Rust's broader alphabetic predicate.
pub(crate) fn is_identifier_start(character: char) -> bool {
    character == '_'
        || matches!(
            get_general_category(character),
            GeneralCategory::UppercaseLetter
                | GeneralCategory::LowercaseLetter
                | GeneralCategory::TitlecaseLetter
                | GeneralCategory::ModifierLetter
                | GeneralCategory::OtherLetter
        )
}

/// Whether `character` may continue an ordinary (non-backticked) Kotlin identifier.
pub(crate) fn is_identifier_continue(character: char) -> bool {
    is_identifier_start(character)
        || get_general_category(character) == GeneralCategory::DecimalNumber
}

/// Getter name for a Kotlin property: `x` -> `getX`; `isOpen` keeps `isOpen`.
pub fn property_getter_name(prop: &str) -> String {
    let b = prop.as_bytes();
    if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() {
        return prop.to_string();
    }
    let mut c = prop.chars();
    match c.next() {
        Some(f) => format!("get{}{}", f.to_uppercase(), c.as_str()),
        None => "get".to_string(),
    }
}

/// Setter name for a Kotlin property: `x` -> `setX`; `isOpen` -> `setOpen`.
pub fn property_setter_name(prop: &str) -> String {
    let b = prop.as_bytes();
    let base = if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() {
        &prop[2..]
    } else {
        prop
    };
    let mut c = base.chars();
    match c.next() {
        Some(f) => format!("set{}{}", f.to_uppercase(), c.as_str()),
        None => "set".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_accessor_names_follow_kotlin_boolean_rules() {
        assert_eq!(property_getter_name("size"), "getSize");
        assert_eq!(property_setter_name("size"), "setSize");
        assert_eq!(property_getter_name("isOpen"), "isOpen");
        assert_eq!(property_setter_name("isOpen"), "setOpen");
        assert_eq!(property_getter_name("island"), "getIsland");
        assert_eq!(property_setter_name("island"), "setIsland");
        assert_eq!(property_getter_name("äpfel"), "getÄpfel");
    }

    #[test]
    fn identifier_categories_match_kotlin() {
        assert!(is_identifier_start('π'));
        assert!(!is_identifier_start('٢'));
        assert!(is_identifier_continue('٢'));
        assert!(!is_identifier_start('Ⅻ'));
        assert!(!is_identifier_continue('²'));
    }
}
