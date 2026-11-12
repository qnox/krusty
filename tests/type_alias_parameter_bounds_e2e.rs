//! kotlinc's BOUND_ON_TYPE_ALIAS_PARAMETER_NOT_ALLOWED: a type alias's type parameter takes no
//! upper bound, whatever the alias's scope, and the error spans the bound from its first token, a
//! type-use annotation included.

use super::common;
use super::diagnostics_parity_support::errors;

#[test]
fn bounds_on_type_alias_parameters_are_rejected_like_kotlinc() {
    let source = "@Target(AnnotationTarget.TYPE)\n\
                  annotation class Anno(val position: String)\n\
                  typealias Top<T : Number> = List<T>\n\
                  class Owner {\n\
                  \x20   typealias Nested<X : Any, Y> = Map<X, Y>\n\
                  }\n\
                  interface Holder<T> {\n\
                  \x20   typealias Annotated<A : @Anno(\"bound\") Number> = Holder<A>\n\
                  }\n\
                  typealias Plain<P> = Set<P>\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("TypeAliasBounds.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        ["bounds on type alias parameters are prohibited."; 3]
    );
}

#[test]
fn bounds_on_local_type_alias_parameters_are_rejected_like_kotlinc() {
    let source = "fun use() {\n\
                  \x20   typealias Local<T : Number> = List<T>\n\
                  \x20   val list: Local<Int> = listOf(1)\n\
                  \x20   println(list)\n\
                  }\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("LocalTypeAliasBounds.kt", source)],
        std::slice::from_ref(&stdlib),
        &["-XXLanguage:+LocalTypeAliases".to_string()],
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        ["bounds on type alias parameters are prohibited."]
    );
}
