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

/// Kotlin's file part short name for a file named `file_stem`, `box` → `BoxKt`: its
/// `PackagePartClassUtils.getFilePartShortName`. It names more than the JVM's file facade: the
/// Compose plugin names each file's `ComposableSingletons$<part>` object after it on every target,
/// whatever `@file:JvmName` says.
pub(crate) fn file_part_short_name(file_stem: &str) -> String {
    let sanitized: String = file_stem
        .chars()
        .map(|character| {
            if character.is_alphabetic() || character.is_numeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    let mut base = String::new();
    let mut characters = sanitized.chars();
    match characters.next() {
        Some(first) if first.is_numeric() => {
            base.push('_');
            base.push_str(&sanitized);
        }
        Some(first) => {
            base.extend(first.to_uppercase());
            base.push_str(characters.as_str());
        }
        None => {}
    }
    base.push_str("Kt");
    base
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
    fn file_part_names_follow_kotlinc_package_part_rules() {
        for (stem, part) in [
            ("box", "BoxKt"),
            ("1", "_1Kt"),
            (
                "32defaultParametersInSuspend",
                "_32defaultParametersInSuspendKt",
            ),
            ("a-b", "A_bKt"),
            ("x.y", "X_yKt"),
            ("q$r", "Q_rKt"),
            ("_u", "_uKt"),
        ] {
            assert_eq!(file_part_short_name(stem), part, "{stem}");
        }
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
