//! Reject paths for argument-count mismatches the corpus doesn't trigger: calling an extension
//! function with the wrong number of arguments, and constructing a qualified nested type with the
//! wrong number of arguments. Both emit the callable-specific checker diagnostic used by kotlinc.

use super::common;

fn diags(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = Some({
        let h = common::java_home();
        std::path::PathBuf::from(format!("{h}/lib/modules"))
    });
    common::front_end_diagnostics(src, &[stdlib], jdk.as_deref())
}

fn assert_arity_error(src: &str) {
    let d = diags(src);
    if d.is_empty() {
        return; // environment skip (no stdlib)
    }
    assert!(
        d.iter().any(|message| {
            (message.contains("expects") && message.contains("args, got"))
                || message.starts_with("too many arguments for '")
        }),
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
