//! Stable type-parameter identities and their source spellings.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::intern;

static TYPE_PARAMETER_SOURCES: OnceLock<Mutex<HashMap<&'static str, &'static str>>> =
    OnceLock::new();

/// Intern one declaration-owned type-parameter identity and retain its source spelling separately.
/// The semantic key is opaque: callers compare it only by identity and never parse declaration
/// coordinates or spelling out of it.
pub(crate) fn declaration_type_parameter(
    compilation: u64,
    file: u32,
    declaration_start: u32,
    index: usize,
    source: &str,
) -> &'static str {
    let semantic = intern(&format!(
        "\0tp:{compilation}:{file}:{declaration_start}:{index}"
    ));
    let source = intern(source);
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(semantic, source);
    semantic
}

/// The call-owned inference variable standing for declaration formal `declared` at one call site.
/// It keeps the declaration's source spelling for diagnostics and never equals a declaration-owned
/// identity, so the enclosing declaration's own `declared` stays a fixed type while the call's
/// variable is solved.
pub(crate) fn call_site_type_variable(declared: &'static str) -> &'static str {
    let fresh = intern(&format!("\0call:{declared}"));
    let mut sources = TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    let source = sources.get(declared).copied().unwrap_or(declared);
    sources.insert(fresh, source);
    fresh
}

/// A constructor-declared type parameter that hides a class type parameter of the same spelling.
/// The JVM signature uses one name for both; they are different inference variables. Diagnostics
/// still print the source spelling.
pub(crate) fn constructor_type_parameter(source: &str) -> &'static str {
    let source = intern(source);
    let semantic = intern(&format!("\0ctor:{source}"));
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(semantic, source);
    semantic
}

/// The source spelling carried by a declaration-scoped semantic type-parameter key. Diagnostics and
/// metadata show the written name even though inference uses the full identity.
pub(crate) fn type_parameter_source_name(name: &str) -> &str {
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(name)
        .copied()
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::{declaration_type_parameter, type_parameter_source_name};

    #[test]
    fn source_spelling_is_not_parsed_from_the_semantic_identity() {
        let semantic = declaration_type_parameter(11, 7, 19, 0, "T$nested");
        assert_eq!(type_parameter_source_name(semantic), "T$nested");
        assert_ne!(semantic, "T$nested");
    }
}
