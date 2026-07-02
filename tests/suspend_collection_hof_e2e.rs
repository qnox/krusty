//! A `suspend` function applying a kotlin-`collections` INLINE HOF or extension (`.map { … }`,
//! `.filter { … }`, `.first()`, `[i]`) to a suspend call's COLLECTION result. Two bugs combined.
//! One, the suspend return was recovered in its erased JVM form `Obj("java/util/List", …)` (the
//! `Continuation<List<T>>` generic signature spells it in Java terms), on which the kotlin.collections
//! extensions aren't keyed — so `.map`/`.first()` didn't resolve; `suspend_return_from_gsig` now
//! canonicalizes a JVM collection to its Kotlin type (`java/util/List` → `kotlin/collections/List`).
//! Two, the CPS `box_returns` pass hit `_ => false` on a LAMBDA argument in a `return m.map { … }`, bailing
//! the whole state machine ("suspend-function shape unsupported") — a lambda argument is a value (its body
//! is a separate impl function, not a `return` of the suspend fn), so it is now a leaf there.
//! Running a coroutine needs a driver, so this asserts the bodies LOWER end-to-end (they were skipped
//! before); the library is built by real kotlinc.
use std::path::PathBuf;
mod common;

const LIB: &str = "package lib\n\
     class Item(val value: String)\n\
     interface Repo {\n\
       suspend fun cfg(): List<Item>\n\
       suspend fun names(): List<String>\n\
     }\n";

#[test]
fn suspend_inline_hof_on_collection_result_compiles() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(libout) = common::compile_lib("susp_hof", LIB) else {
        return;
    };
    let cp: Vec<PathBuf> = vec![libout, sl];
    for (tag, main) in [
        (
            "map_bound",
            "import lib.Repo\nsuspend fun f(r: Repo): List<String> { val m = r.cfg(); return m.map { it.value } }\nfun box(): String = \"OK\"\n",
        ),
        (
            "map_expr_body",
            "import lib.Repo\nsuspend fun f(r: Repo): List<String> = r.cfg().map { it.value }\nfun box(): String = \"OK\"\n",
        ),
        (
            "filter_bound",
            "import lib.Repo\nsuspend fun f(r: Repo): List<lib.Item> { val m = r.cfg(); return m.filter { it.value != \"\" } }\nfun box(): String = \"OK\"\n",
        ),
        (
            "map_then_filter_chain",
            "import lib.Repo\nsuspend fun f(r: Repo): List<String> { val m = r.cfg(); return m.map { it.value }.filter { it.isNotEmpty() } }\nfun box(): String = \"OK\"\n",
        ),
        (
            "first_on_result",
            "import lib.Repo\nsuspend fun f(r: Repo): String = r.cfg().first().value\nfun box(): String = \"OK\"\n",
        ),
        (
            "index_on_bound",
            "import lib.Repo\nsuspend fun f(r: Repo): String { val m = r.cfg(); return m[0].value }\nfun box(): String = \"OK\"\n",
        ),
        (
            "map_string_list",
            "import lib.Repo\nsuspend fun f(r: Repo): List<Int> { val n = r.names(); return n.map { it.length } }\nfun box(): String = \"OK\"\n",
        ),
    ] {
        assert!(
            common::compile_in_process(main, "Main", &cp, Some(&jdk)).is_some(),
            "{tag}: suspend inline-HOF on a collection result should lower"
        );
    }
}
