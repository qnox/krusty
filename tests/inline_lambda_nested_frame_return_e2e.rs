//! A return inside a lambda passed to an inline function, nested in another inline expansion, that
//! leaves that enclosing expansion. The nested lambda body numbers its values separately, so its
//! return stores the expansion's result by naming the expansion's frame. The frame may be an
//! inlined lambda (`return@outer`) or an inline function (`return`), from the classpath or from
//! this file, and may sit inside a `try`/`finally` whose finalizer still runs. The inline functions
//! the lambdas are passed to are this repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <T, R> withIt(value: T, block: (T) -> R): R = block(value)
inline fun <T, R> T.myLet(block: (T) -> R): R = block(this)

class Gate {
    var held = false
    fun lock() { held = true }
    fun unlock() { held = false }
}

inline fun <T> Gate.guarded(action: () -> T): T {
    lock()
    try {
        return action()
    } finally {
        unlock()
    }
}
"#;

const MAIN: &str = r#"
inline fun <R> ownRun(block: () -> R): R = block()

inline fun ownPick(a: String?, b: Int): String {
    val local = b + 1
    a?.myLet { return it + local }
    return "none$local"
}

fun classpathFrame(a: String?): String = withIt(1) {
    a?.myLet { return@withIt it }
    "none"
}

val gate = Gate()

fun guardedFrame(a: String?, b: String?): String = gate.guarded {
    a?.myLet { return@guarded it }
    b?.myLet { return@guarded it }
    "none"
}

fun sameFileFrame(a: String?, b: Int): String {
    val prefix = "p$b"
    return prefix + ownRun {
        val local = b + 1
        a?.myLet { return@ownRun it + local }
        "none$local"
    }
}

fun sameFileFunction(a: String?, b: Int): String {
    val prefix = "p$b"
    return prefix + ownPick(a, b)
}

fun box(): String {
    val classpath = classpathFrame("O") + classpathFrame(null)
    if (classpath != "Onone") return "classpath frame: $classpath"
    val guarded = guardedFrame("A", null) + guardedFrame(null, "B") + guardedFrame(null, null)
    if (guarded != "ABnone" || gate.held) return "guarded frame: $guarded ${gate.held}"
    val lambda = sameFileFrame("O", 1) + sameFileFrame(null, 2)
    if (lambda != "p1O2p2none3") return "same-file frame: $lambda"
    val function = sameFileFunction("O", 1) + sameFileFunction(null, 2)
    if (function != "p1O2p2none3") return "same-file function: $function"
    return "OK"
}
"#;

#[test]
fn a_nested_lambda_returns_from_its_enclosing_expansion() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
