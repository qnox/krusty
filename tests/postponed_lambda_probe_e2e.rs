//! A generic call whose lambda parameter's INPUT type is one of the callee's own formals
//! (`fun <T : Any> matching(f: (T) -> Boolean): T`) can only shape that lambda once `T` is known.
//! In argument position the outer call supplies `T` after it selects, and krusty already
//! re-checks the nested call under that expectation; but the first applicability probe checked
//! the lambda body against the open `T` and REPORTED what it found (`unresolved reference
//! 'contains'` on `it`). kotlinc postpones such a lambda: the body is analyzed once `T` is
//! fixed, and a `T` nothing fixes is reported at the call as uninferable. mockk's
//! `match { it.contains(…) }` inside a `verify { mail.send(subject = match { … }) }` is the
//! everyday shape. Measured against kotlinc 2.4.10.

use super::common;

const LIB: &str = "fun <T : Any> matching(f: (T) -> Boolean): T = throw IllegalStateException()\n\
class Scope { fun <T : Any> m(f: (T) -> Boolean): T = throw IllegalStateException() }\n\
interface Mail { fun send(to: String, subject: String, html: String): Boolean }\n\
class Recorder : Mail {\n\
    var last = \"\"\n\
    override fun send(to: String, subject: String, html: String): Boolean { last = \"$to/$subject/$html\"; return true }\n\
}\n";

#[test]
fn a_postponed_lambda_is_typed_by_the_outer_parameter_without_probe_noise() {
    let src = format!(
        "{LIB}\
fun probe(mail: Mail, scope: Scope, block: Scope.() -> String): String {{\n\
    val positional = runCatching {{ mail.send(matching {{ it.contains(\"a\") }}, \"s\", \"h\") }}.isFailure\n\
    val named = runCatching {{ mail.send(to = \"x\", subject = matching {{ it.length > 1 }}, html = \"h\") }}.isFailure\n\
    val member = runCatching {{ mail.send(scope.m {{ it.contains(\"a\") }}, \"s\", \"h\") }}.isFailure\n\
    val implicit = runCatching {{ scope.block() }}.isFailure\n\
    return if (positional && named && member && implicit) \"OK\" else \"FAIL\"\n\
}}\n\
fun box(): String = probe(Recorder(), Scope()) {{ m {{ it.contains(\"a\") }} }}\n"
    );
    let diagnostics = common::front_end_diagnostics_files_with_stdlib(&[&src]);
    assert_eq!(diagnostics, Vec::<String>::new());
    assert_eq!(
        common::compile_and_run_with_stdlib(&src, "Main").expect("postponed lambdas compile + run"),
        "OK"
    );
}

#[test]
fn an_unbound_lambda_input_is_still_reported_in_statement_position() {
    // Nothing fixes `T` here and no enclosing call re-checks the lambda: the held probe verdict
    // is the verdict, reported at statement end. (kotlinc additionally reports the call itself
    // as uninferable, `cannot infer type for type parameter 'T'`; that diagnostic is still open
    // in krusty for a top-level callee, which reports nothing at all for this shape.)
    let src = format!("{LIB}fun f(scope: Scope) {{\n    scope.m {{ it.foo() }}\n}}\n");
    let diagnostics = common::front_end_diagnostics_files_with_stdlib(&[&src]);
    assert_eq!(diagnostics, vec!["unresolved reference 'foo'.".to_string()]);
}
