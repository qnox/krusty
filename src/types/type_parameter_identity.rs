//! Stable type-parameter identities and their source spellings.

use crate::fx_hash::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use super::intern;

static TYPE_PARAMETER_SOURCES: OnceLock<Mutex<HashMap<&'static str, &'static str>>> =
    OnceLock::new();
static NEXT_METADATA_TYPE_PARAMETER: AtomicU64 = AtomicU64::new(0);
static NESTED_RESULT_TYPE_VARIABLES: OnceLock<Mutex<crate::fx_hash::HashSet<&'static str>>> =
    OnceLock::new();
static CALL_SITE_VARIABLES: OnceLock<Mutex<HashMap<&'static str, CallSiteVariable>>> =
    OnceLock::new();
static POSTPONED_VARIABLE_ORIGINS: OnceLock<Mutex<HashMap<&'static str, PostponedVariableOrigin>>> =
    OnceLock::new();

/// Which formal of the called declaration a postponed-call inference variable stands for, and the
/// function arguments of its call that were analysed under it (by source span). A declaration those
/// arguments published before the call was solved (a retained capture) completes its type from the
/// solution signature inference reached for exactly these arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PostponedVariableOrigin {
    pub(crate) formal: super::TyParamName,
    pub(crate) file: u32,
    pub(crate) arguments: Vec<crate::diag::Span>,
}

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

/// One call expression, the owner of the inference variables that call solves.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CallSiteIdentity {
    pub(crate) compilation: u64,
    pub(crate) file: u32,
    pub(crate) call: u32,
}

/// The call and declaration formal an inference variable minted by [`call_site_type_variable`]
/// stands for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CallSiteVariable {
    pub(crate) site: CallSiteIdentity,
    pub(crate) formal: &'static str,
}

/// The inference variable that call `site` solves for declaration formal `declared`. Every call
/// owns its own: two calls to one declaration, nested or recursive, never share a variable, and a
/// variable never equals the declaration-owned identity the enclosing declaration may have in
/// scope. It keeps the declaration's source spelling for diagnostics.
pub(crate) fn call_site_type_variable(
    site: CallSiteIdentity,
    declared: &'static str,
) -> &'static str {
    let CallSiteIdentity {
        compilation,
        file,
        call,
    } = site;
    let fresh = intern(&format!("\0call:{compilation}:{file}:{call}:{declared}"));
    {
        let mut sources = TYPE_PARAMETER_SOURCES
            .get_or_init(|| Mutex::new(HashMap::default()))
            .lock()
            .unwrap();
        let source = sources.get(declared).copied().unwrap_or(declared);
        sources.insert(fresh, source);
    }
    CALL_SITE_VARIABLES
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap()
        .insert(
            fresh,
            CallSiteVariable {
                site,
                formal: declared,
            },
        );
    fresh
}

/// The variable an enclosing call solves for a nested call's formal that the nested call's own
/// inputs left unfixed (`update(column, wrap("x"))` with `fun <T : Any> wrap(label: String)`). The
/// nested call's provisional result names it in place of the declaration formal, so enclosing
/// overload selection reads that argument as a result-only producer rather than as a fixed type.
pub(crate) fn nested_result_type_variable(declared: &'static str) -> &'static str {
    let fresh = intern(&format!("\0nested-result:{declared}"));
    let mut sources = TYPE_PARAMETER_SOURCES
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap();
    let source = sources.get(declared).copied().unwrap_or(declared);
    sources.insert(fresh, source);
    NESTED_RESULT_TYPE_VARIABLES
        .get_or_init(|| Mutex::new(crate::fx_hash::HashSet::default()))
        .lock()
        .unwrap()
        .insert(fresh);
    fresh
}

/// Whether `identity` was produced by [`nested_result_type_variable`].
pub(crate) fn is_nested_result_type_variable(identity: &str) -> bool {
    NESTED_RESULT_TYPE_VARIABLES
        .get()
        .is_some_and(|variables| variables.lock().unwrap().contains(identity))
}

/// `ty` with call `site`'s own inference variables expressed as the formals they stand for. A call
/// publishes its selection over the called declaration's formals; another call's variable is not
/// this call's to translate and stays.
pub(crate) fn ty_declared_at_call(ty: super::Ty, site: CallSiteIdentity) -> super::Ty {
    super::ty_map_symbols(ty, &|symbol, bound| match symbol.kind() {
        super::TyKind::Var(name, _) => match call_site_variable(name) {
            Some(variable) if variable.site == site => super::Ty::ty_param(variable.formal, bound),
            _ => symbol.with_symbol_bound(bound),
        },
        _ => symbol.with_symbol_bound(bound),
    })
}

/// The call and formal behind a call's inference variable; `None` for every other identity.
pub(crate) fn call_site_variable(variable: super::TyParamName) -> Option<CallSiteVariable> {
    CALL_SITE_VARIABLES
        .get()?
        .lock()
        .unwrap()
        .get(variable.as_str())
        .copied()
}

/// The inference variable call `site` solves for `declared`, entered by one of its function
/// arguments before the call is solved. It is the same variable [`call_site_type_variable`] gives
/// the call's candidate selection, so the argument's checked type and the selected signature speak
/// about one variable. It also records `argument` as one of the function arguments analysed under
/// the variable ([`postponed_variable_origin`]).
pub(crate) fn postponed_call_type_variable(
    site: CallSiteIdentity,
    argument: crate::diag::Span,
    declared: &str,
) -> &'static str {
    let declared = intern(declared);
    let fresh = call_site_type_variable(site, declared);
    let file = site.file;
    let mut origins = POSTPONED_VARIABLE_ORIGINS
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap();
    let origin = origins
        .entry(fresh)
        .or_insert_with(|| PostponedVariableOrigin {
            formal: super::TyParamName::new(declared),
            file,
            arguments: Vec::new(),
        });
    if !origin.arguments.contains(&argument) {
        origin.arguments.push(argument);
    }
    fresh
}

/// The call and formal behind a postponed-call inference variable; `None` for every other identity.
pub(crate) fn postponed_variable_origin(
    variable: super::TyParamName,
) -> Option<PostponedVariableOrigin> {
    POSTPONED_VARIABLE_ORIGINS
        .get()?
        .lock()
        .unwrap()
        .get(variable.as_str())
        .cloned()
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

/// An enclosing class's formal that a member class captures but hides behind a nearer formal of the
/// same spelling (`class Outer<T> { class Inner<T> }`). Java names both `T`; the captured one still
/// takes its slot in `Outer<A>.Inner<B>` but no member can name it, so it gets an identity of its
/// own. `owner` is the capturing class and `ordinal` the slot; neither is interpreted.
pub(crate) fn hidden_captured_type_parameter(
    owner: super::TypeName,
    ordinal: usize,
    source: &str,
) -> &'static str {
    let source = intern(source);
    let semantic = intern(&format!(
        "\0hidden-captured:{}:{ordinal}",
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
        call_site_type_variable, constructor_type_parameter, declaration_type_parameter,
        postponed_call_type_variable, postponed_variable_origin, type_parameter_source_name,
        CallSiteIdentity, PostponedVariableOrigin,
    };
    use crate::diag::Span;
    use crate::types::TyParamName;

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
        let outer_block = Span::new(30, 60);
        let outer_trailing = Span::new(62, 90);
        let inner_block = Span::new(40, 50);
        let site = |call| CallSiteIdentity {
            compilation: 23,
            file: 1,
            call,
        };
        let outer = postponed_call_type_variable(site(40), outer_block, declared);
        let same_outer = postponed_call_type_variable(site(40), outer_trailing, declared);
        let inner = postponed_call_type_variable(site(55), inner_block, declared);

        assert_eq!(outer, same_outer);
        assert_eq!(outer, call_site_type_variable(site(40), declared));
        assert_ne!(outer, inner);
        assert_ne!(outer, declared);
        assert_eq!(type_parameter_source_name(outer), "T");
        assert_eq!(type_parameter_source_name(inner), "T");
        let origin = |variable| postponed_variable_origin(TyParamName::new(variable));
        assert_eq!(
            origin(outer),
            Some(PostponedVariableOrigin {
                formal: TyParamName::new(declared),
                file: 1,
                arguments: vec![outer_block, outer_trailing],
            })
        );
        assert_eq!(
            origin(inner).map(|origin| origin.arguments),
            Some(vec![inner_block])
        );
        assert_eq!(origin(declared), None);
    }
}
