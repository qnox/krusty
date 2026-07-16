//! Kotlin source/metadata naming helpers shared across compiler phases.

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
}
