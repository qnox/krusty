//! A suspension inside a lambda passed to a CLASSPATH `inline` function.
//!
//! Once the call is spliced, that lambda's body runs in the enclosing method's own frame, so the
//! suspension belongs to the enclosing coroutine. The IR machine cannot see it — it stops at every
//! lambda — so the enclosing function was classified as having no suspension point at all and the
//! spliced call was emitted with no continuation to pass:
//!
//! ```text
//! error: krusty: JVM backend inline error: call arity mismatch
//! ```
//!
//! The machine for these is built during emission instead, where the spliced body's own locals
//! exist and can be spilled. See `docs/JVM_INLINE_BEFORE_CPS.md`.

use super::common;

/// Compile `main` against a reference-compiled inline library, with coroutines on the classpath.
fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let libout = common::compile_lib_ref(tag, LIB)?;
    let cp = [
        libout,
        common::stdlib_jar(),
        common::coroutines_jar(),
        jdk.clone(),
    ];
    Some(common::expect_box_run(
        main,
        "Main",
        &cp,
        Some(jdk.as_path()),
    ))
}

/// A classpath inline function with a loop and accumulator locals — none of which exist until it
/// has been spliced, which is exactly what the machine has to spill.
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

#[test]
fn a_suspension_inside_a_spliced_inline_lambda_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun many(v: Int): Int = twice(v) { one(it) }
        fun box(): String = runBlocking {
            val n = many(10)
            if (n == 23) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_inline", MAIN) else {
        return; // toolchain not provisioned
    };
    assert_eq!(output, "OK");
}
