//! An inline function parameter receives a callable reference as the call its adapter returns,
//! not as a function-reference object. The referenced declaration's kind is not what makes the
//! argument spliceable.

use super::common;

const SRC: &str = "\
class Holder(val name: String)\n\
\n\
fun makeHolder(name: String) = Holder(name)\n\
\n\
inline fun <T, R> applyEach(value: T, transform: (T) -> R): R = transform(value)\n\
\n\
fun unbound(name: String) = applyEach(name, ::makeHolder)\n\
\n\
class Box(val prefix: String) {\n\
    fun label(name: String) = prefix + name\n\
}\n\
\n\
fun bound(box: Box, name: String) = applyEach(name, box::label)\n\
\n\
fun local(prefix: String, name: String): String {\n\
    fun label(value: String) = prefix + value\n\
    return applyEach(name, ::label)\n\
}\n\
\n\
fun names(values: List<String>): List<Holder> = values.map(::Holder)\n\
\n\
fun mapped(values: List<String>): List<Holder> = values.map(::makeHolder)\n\
";

fn method_matches(method: &str) {
    let difference = common::method_code_diff_against_kotlinc(
        "InlineCtorRef",
        &[],
        SRC,
        "InlineCtorRefKt",
        method,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(difference, Ok(()));
}

#[test]
fn an_inline_map_of_a_constructor_reference_matches_kotlinc() {
    method_matches("public static final java.util.List<Holder> names(");
}

#[test]
fn an_inline_map_of_a_function_reference_matches_kotlinc() {
    method_matches("public static final java.util.List<Holder> mapped(");
}

#[test]
fn an_unbound_function_reference_in_an_inline_call_matches_kotlinc() {
    method_matches("public static final Holder unbound(");
}

#[test]
fn a_bound_reference_in_an_inline_call_matches_kotlinc() {
    method_matches("public static final java.lang.String bound(");
}

#[test]
fn a_local_function_reference_in_an_inline_call_matches_kotlinc() {
    method_matches("public static final java.lang.String local(");
}
