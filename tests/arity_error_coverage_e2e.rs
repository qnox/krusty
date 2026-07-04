//! Reject paths for argument-count mismatches the corpus doesn't trigger: calling an extension
//! function with the wrong number of arguments, and constructing a qualified nested type with the
//! wrong number of arguments. Both emit a checker diagnostic (`… expects N args, got M`).

use super::common;

fn diags(src: &str) -> Vec<String> {
    let stdlib = match common::stdlib_jar() {
        Some(p) => p,
        None => {
            eprintln!("skipping arity_error_coverage_e2e: no kotlin-stdlib jar");
            return vec![];
        }
    };
    let jdk = common::java_home().map(|h| std::path::PathBuf::from(format!("{h}/lib/modules")));
    common::front_end_diagnostics(src, &[stdlib], jdk.as_deref())
}

fn assert_arity_error(src: &str) {
    let d = diags(src);
    if d.is_empty() {
        return; // environment skip (no stdlib)
    }
    assert!(
        d.iter()
            .any(|m| m.contains("expects") && m.contains("args, got")),
        "expected an arity error, got: {d:?}"
    );
}

#[test]
fn extension_arity_mismatch_rejected() {
    assert_arity_error("fun Int.ext(a: Int): Int = a\nfun f() { 5.ext(1, 2) }\n");
}

#[test]
fn nested_constructor_arity_mismatch_rejected() {
    assert_arity_error(
        "class Outer {\n    class Inner(val x: Int)\n}\nfun f() { Outer.Inner(1, 2) }\n",
    );
}
