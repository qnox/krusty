//! Stable type-parameter identities and their source spellings.

use crate::fx_hash::HashMap;
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
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap()
        .insert(semantic, source);
    semantic
}

/// A type parameter owned by one declaration in an external provider instance.
///
/// External callable IDs are stable only within their provider, so `provider` remains part of the
/// opaque key. The source spelling is retained separately for diagnostics and never participates in
/// equality after this boundary.
pub(crate) fn dependency_callable_type_parameter(
    provider: u64,
    declaration: u32,
    ordinal: usize,
    source: &str,
) -> &'static str {
    let semantic = intern(&format!(
        "\0dependency-tp:{provider}:{declaration}:{ordinal}"
    ));
    let source = intern(type_parameter_source_name(source));
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap()
        .insert(semantic, source);
    semantic
}

/// A type parameter owned by one property declaration in an external provider instance.
///
/// A property's getter and setter are distinct callable realizations, but the generic receiver and
/// result belong to the one source property. Keeping a separate key space lets both accessors carry
/// the property's identity without conflating the property with either physical method.
pub(crate) fn dependency_property_type_parameter(
    provider: u64,
    declaration: u32,
    ordinal: usize,
    source: &str,
) -> &'static str {
    let semantic = intern(&format!(
        "\0dependency-property-tp:{provider}:{declaration}:{ordinal}"
    ));
    let source = intern(type_parameter_source_name(source));
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::default()))
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
        .get_or_init(|| Mutex::new(HashMap::default()))
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
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap();
    let source = sources.get(declared).copied().unwrap_or(declared);
    sources.insert(fresh, source);
    fresh
}

/// The inference variable owned by one postponed call expression. Unlike
/// [`call_site_type_variable`], this identity includes the source call: nested invocations of the
/// same generic builder are simultaneously active and must not share their variables.
pub(crate) fn postponed_call_type_variable(
    compilation: u64,
    file: u32,
    expression: u32,
    declared: &str,
) -> &'static str {
    let declared = intern(declared);
    let fresh = intern(&format!(
        "\0postponed-call:{compilation}:{file}:{expression}:{declared}"
    ));
    let mut sources = TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::default()))
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
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap()
        .insert(semantic, source);
    semantic
}

/// A type parameter a compiler plugin declares on a function it generates for `owner`, copying one
/// of the owner's own. It never equals the owner's parameter it copies: the function binds its own.
/// `function` is the producer's stable name for the generated function and is not interpreted.
pub(crate) fn generated_function_type_parameter(
    owner: super::TypeName,
    function: &str,
    ordinal: usize,
    source: &str,
) -> &'static str {
    let source = intern(source);
    let semantic = intern(&format!(
        "\0generated-fn:{}:{function}:{ordinal}",
        owner.name_id().0
    ));
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap()
        .insert(semantic, source);
    semantic
}

/// The source spelling carried by a declaration-scoped semantic type-parameter key. Diagnostics and
/// metadata show the written name even though inference uses the full identity.
pub(crate) fn type_parameter_source_name(name: &str) -> &str {
    TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap()
        .get(name)
        .copied()
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::{
        constructor_type_parameter, declaration_type_parameter, postponed_call_type_variable,
        type_parameter_source_name,
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

    #[test]
    fn nested_postponed_calls_own_distinct_variables() {
        let declared = declaration_type_parameter(23, 1, 17, 0, "T");
        let outer = postponed_call_type_variable(23, 1, 40, declared);
        let same_outer = postponed_call_type_variable(23, 1, 40, declared);
        let inner = postponed_call_type_variable(23, 1, 55, declared);

        assert_eq!(outer, same_outer);
        assert_ne!(outer, inner);
        assert_ne!(outer, declared);
        assert_eq!(type_parameter_source_name(outer), "T");
        assert_eq!(type_parameter_source_name(inner), "T");
    }
}
