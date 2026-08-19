//! Signature inference binds callable type variables from lambda result types.

use super::common;

fn assert_frontends_accept(name: &str, src: &str) {
    let (reference_code, reference_stderr) = common::kotlinc_source_result(name, src);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected {name}: {reference_stderr}"
    );
    let diagnostics = common::front_end_diagnostics_files_with_stdlib(&[src]);
    assert_eq!(diagnostics.len(), 0, "{name}: {diagnostics:?}");
    assert_eq!(diagnostics, Vec::<String>::new(), "{name}");
}

#[test]
fn toplevel_generic_call_binds_type_param_from_lambda_result() {
    assert_frontends_accept(
        "top-level make { 1 }",
        "fun <T> make(f: () -> T): T = f()\nval x = make { 1 }\nval y = x + 1\n",
    );
}

#[test]
fn member_generic_call_binds_type_param_from_lambda_result() {
    assert_frontends_accept(
        "member make { 1 }",
        "fun <T> make(f: () -> T): T = f()\nclass C {\n    val x = make { 1 }\n    val y = x + 1\n}\n",
    );
}

#[test]
fn toplevel_lazy_delegate_infers_value_type_from_lambda_result() {
    assert_frontends_accept(
        "top-level by lazy",
        "val x by lazy { 1 }\nfun g(i: Int) = i\nval y = g(x)\nval z = x + 1\n",
    );
}

#[test]
fn member_lazy_delegate_infers_value_type_from_lambda_result() {
    assert_frontends_accept(
        "member by lazy",
        "class C {\n    val x by lazy { 1 }\n    fun g(i: Int) = i\n    val y = g(x)\n}\n",
    );
}
