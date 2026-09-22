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

/// A dependency that wraps its own `try` AROUND the lambda call. Both handlers then cover the
/// spliced body, and the JVM takes the FIRST table entry that covers the pc and matches the thrown
/// type (JVMS 2.10) — so the lambda's entry has to be listed before the host's. Relocated in the
/// order they were collected (host first, lambda appended), the host's `catch` silently won:
/// `guard`'s arm ran and returned -99 where the lambda's own `catch` should have returned 7. No
/// verification error and no diagnostic — the wrong answer was simply computed.
const GUARD_LIB: &str = r#"
inline fun guard(f: () -> Int): Int {
    return try { f() } catch (e: Throwable) { -99 }
}
"#;

#[test]
fn a_host_handler_around_the_call_does_not_shadow_the_lambdas_own_catch() {
    const MAIN: &str = r#"
        fun box(): String {
            val n = guard {
                try { throw IllegalStateException("inner"); 0 } catch (e: IllegalStateException) { 7 }
            }
            return if (n == 7) "OK" else "FAIL: " + n
        }
    "#;
    let jdk = common::jdk_modules();
    let Some(libout) = common::compile_lib_ref("spliced_lambda_try_host_guard", GUARD_LIB) else {
        return;
    };
    let cp = [libout, common::stdlib_jar(), jdk.clone()];
    assert_eq!(
        common::expect_box_run(MAIN, "Main", &cp, Some(jdk.as_path())),
        "OK"
    );
}

/// The host's handler must still fire for what the lambda does NOT catch.
#[test]
fn a_host_handler_still_catches_what_the_lambda_does_not() {
    const MAIN: &str = r#"
        fun box(): String {
            val n = guard {
                try { throw java.io.IOException("inner"); 0 } catch (e: IllegalStateException) { 7 }
            }
            return if (n == -99) "OK" else "FAIL: " + n
        }
    "#;
    let jdk = common::jdk_modules();
    let Some(libout) = common::compile_lib_ref("spliced_lambda_try_host_guard", GUARD_LIB) else {
        return;
    };
    let cp = [libout, common::stdlib_jar(), jdk.clone()];
    assert_eq!(
        common::expect_box_run(MAIN, "Main", &cp, Some(jdk.as_path())),
        "OK"
    );
}
