//! A `try` inside a lambda passed to a CLASSPATH `inline` function.
//!
//! Once the lambda's body is spliced, its `try` ranges are code inside another method and have to
//! reach that method's exception table. The splice used to relocate only the DEPENDENCY's own
//! handlers, so a `catch` written in the lambda was dead code: the exception escaped, silently.

use super::common;

const LIB: &str = r#"
inline fun twice(x: Int, f: (Int) -> Int): Int {
    var acc = 0
    var i = 0
    while (i < 2) {
        val r = f(x + i)
        acc = acc + r
        i = i + 1
    }
    return acc
}
"#;

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let libout = common::compile_lib_ref(tag, LIB)?;
    let cp = [libout, common::stdlib_jar(), jdk.clone()];
    Some(common::expect_box_run(
        main,
        "Main",
        &cp,
        Some(jdk.as_path()),
    ))
}

#[test]
fn a_try_inside_a_spliced_lambda_keeps_its_handler() {
    const MAIN: &str = r#"
        fun guarded(): Int = twice(1) { x ->
            try { if (x == 2) throw IllegalStateException("x"); x } catch (e: Exception) { -1 }
        }
        fun box(): String {
            val n = guarded()
            return if (n == 0) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("spliced_lambda_try", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A `finally` is a catch-all handler plus an inlined copy on the normal path; both must survive.
#[test]
fn a_try_finally_inside_a_spliced_lambda_runs_both_paths() {
    const MAIN: &str = r#"
        var trace = ""
        fun guarded(): Int = twice(1) { x ->
            try {
                if (x == 2) throw IllegalStateException("x")
                x
            } catch (e: Exception) {
                -1
            } finally {
                trace = trace + x
            }
        }
        fun box(): String {
            val n = guarded()
            return if (n == 0 && trace == "12") "OK" else "FAIL: " + n + " " + trace
        }
    "#;
    let Some(output) = run("spliced_lambda_try_finally", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}
