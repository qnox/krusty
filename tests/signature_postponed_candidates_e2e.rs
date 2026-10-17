//! An inferred declaration type whose call has a postponed lambda is shaped only by the call
//! candidates that can take that lambda.
//!
//! Pass 1 types `fun run() = lib.launch { ... }` and `val engine = Engine { ... }` before their
//! bodies, so the lambda needs its parameter shape from the call's candidates. Two shapes declined
//! and reported "module signatures were not finalized": a package-qualified call whose other
//! overload needs one more argument made the family ambiguous, and a constructor whose parameter
//! cannot take a lambda claimed the call ahead of the companion `invoke` it selects.

use super::common;

const LIBRARY: &str = r#"
package lib

class Scope {
    val log = StringBuilder()
    fun mark(step: String) { log.append(step) }
}

fun launch(retries: Int = 0, body: Scope.() -> Unit): String = Scope().apply(body).log.toString()
fun launch(timeoutMillis: Long, body: Scope.() -> Unit): String = "timeout $timeoutMillis"

class Config(val name: String)

class Engine(val config: Config) {
    constructor(config: Config, verbose: Boolean) : this(config)

    companion object {
        operator fun invoke(handler: Scope.(String) -> Unit): Engine =
            Engine(Config(Scope().apply { handler("engine") }.log.toString()))
    }
}
"#;

const MAIN: &str = r#"
import lib.Engine

class Runner {
    fun run() = lib.launch { mark("run") }
    private val engine = Engine { name -> mark(name) }
    fun engineName() = engine.config.name
}

val topLevel = lib.launch { mark("top") }
val topEngine = Engine { mark("top-$it") }

fun box(): String {
    if (Runner().run() != "run") return "run"
    if (Runner().engineName() != "engine") return "engine"
    if (topLevel != "top") return "top"
    if (topEngine.config.name != "top-engine") return "top engine"
    return "OK"
}
"#;

#[test]
fn postponed_lambdas_are_shaped_by_the_candidates_that_take_them() {
    let sources = [("Library.kt", LIBRARY), ("Main.kt", MAIN)];
    common::assert_sources_accepted_like_kotlinc(&sources, &[]);
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for (class, reference) in &classes.reference {
        assert!(
            classes.krusty[class] == *reference,
            "{class} differs from kotlinc"
        );
    }
}
