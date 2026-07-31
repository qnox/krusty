//! Kotlin source/metadata naming helpers shared across compiler phases.

/// Return every JVM internal-name spelling obtained by progressively treating trailing path
/// segments as nested classifiers. The package-first order is deliberate: callers that merely need
/// the first existing binary name use `a/b/C`, `a/b$C`, `a$b$C`, while Kotlin source lookup reverses
/// it because a classifier prefix shadows an otherwise identical package prefix.
pub(crate) fn nested_internal_name_candidates(internal: &str) -> Vec<String> {
    let mut candidates =
        Vec::with_capacity(internal.bytes().filter(|&byte| byte == b'/').count() + 1);
    let mut candidate = internal.to_string();
    candidates.push(candidate.clone());
    while let Some(separator) = candidate.rfind('/') {
        candidate.replace_range(separator..=separator, "$");
        candidates.push(candidate.clone());
    }
    candidates
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
    }

    #[test]
    fn nested_internal_candidates_replace_separators_from_the_right() {
        assert_eq!(
            nested_internal_name_candidates("a/b/C"),
            ["a/b/C", "a/b$C", "a$b$C"]
        );
        assert_eq!(nested_internal_name_candidates("C"), ["C"]);
    }
}
