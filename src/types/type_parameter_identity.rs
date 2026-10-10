//! Stable type-parameter identities and their source spellings.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use super::intern;

static TYPE_PARAMETER_SOURCES: OnceLock<Mutex<HashMap<&'static str, &'static str>>> =
    OnceLock::new();
static NEXT_METADATA_TYPE_PARAMETER: AtomicU64 = AtomicU64::new(0);

/// Allocate an opaque identity for a type parameter owned by a dependency declaration. The
/// provider retains the identity beside the exact declaration key it decoded; only the source
/// spelling is registered here for diagnostics.
pub(crate) fn metadata_type_parameter(source: &str) -> &'static str {
    let ordinal = NEXT_METADATA_TYPE_PARAMETER.fetch_add(1, Ordering::Relaxed);
    let semantic = intern(&format!("\0metadata-tp:{ordinal}"));
    let source = intern(source);
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(semantic, source);
    semantic
}

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
/// The JVM signature uses one name for both; they are different inference variables. `declaration`
/// is the caller's stable constructor identity (owner alone cannot separate overloads) and is not
/// interpreted. Diagnostics still print the source spelling.
pub(crate) fn constructor_type_parameter(
    owner: super::TypeName,
    declaration: &str,
    ordinal: usize,
    source: &str,
) -> &'static str {
    let source = intern(source);
    let semantic = intern(&format!(
        "\0ctor:{}:{declaration}:{ordinal}",
        owner.name_id().0
    ));
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
    use super::{
        constructor_type_parameter, declaration_type_parameter, type_parameter_source_name,
    };

    #[test]
    fn source_spelling_is_not_parsed_from_the_semantic_identity() {
        let semantic = declaration_type_parameter(11, 7, 19, 0, "T$nested");
        assert_eq!(type_parameter_source_name(semantic), "T$nested");
        assert_ne!(semantic, "T$nested");
    }

    #[test]
    fn constructor_formals_that_share_a_spelling_stay_distinct() {
        let left = crate::types::type_name("demo/Left");
        let right = crate::types::type_name("demo/Right");
        let first = constructor_type_parameter(left, "(I)V", 0, "T");
        let overload = constructor_type_parameter(left, "(Ljava/lang/String;)V", 0, "T");
        let other_owner = constructor_type_parameter(right, "(I)V", 0, "T");
        let next_formal = constructor_type_parameter(left, "(I)V", 1, "T");

        assert_ne!(first, overload);
        assert_ne!(first, other_owner);
        assert_ne!(first, next_formal);
        assert_eq!(type_parameter_source_name(first), "T");
        assert_eq!(type_parameter_source_name(overload), "T");
        assert_eq!(type_parameter_source_name(other_owner), "T");
        assert_eq!(type_parameter_source_name(next_formal), "T");
    }
}
