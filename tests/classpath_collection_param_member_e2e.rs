//! A classpath (jar) method or interface member whose PARAMETER is a Kotlin collection (`List<String>`,
//! `List<Int>`, `List<CustomType>`) failed member resolution → `unresolved method '…' on 'lib/…'`. The JVM
//! method descriptor erases a collection parameter to its single JVM interface with the type argument
//! dropped (`List<String>` → `Ljava/util/List;`), but the call passes the Kotlin type itself
//! (`h.size(listOf("a"))` → arg `kotlin/collections/List<String>`); the exact / `Any`-widened / subtype
//! overload passes all compared `java/util/List` against `kotlin/collections/List<String>` and missed.
//! `select_instance_info` now has a final pass matching BOTH parameter and argument in their JVM-descriptor
//! form (mirroring the constructor path `resolve_constructor` already had — this is its method analog),
//! bridging the collection identity and erasing type arguments.
//!
//! This single root covered TWO reported bugs: z1 (any method with a `List<T>` param, no suspend) and w1
//! (a `suspend` interface method returning `List<CustomType>` — its `get(ids: List<Int>)` param was the
//! actual failure, not the return). The suspend case is asserted to LOWER end-to-end (a coroutine RUN needs
//! a driver); the non-suspend cases run their `box()` on the JVM.
use super::common;
use std::path::PathBuf;

fn run_with_lib(tag: &str, lib: &str, main: &str) -> Option<String> {
    common::run_box_against(tag, lib, main)
}

#[test]
fn method_with_list_string_param() {
    // z1: `List<String>` parameter on a classpath class — two members, both resolve + run.
    const LIB: &str = "package lib\n\
        class H {\n\
        \x20 fun size(items: List<String>): Int = items.size\n\
        \x20 fun join(items: List<String>): String = items.joinToString(\",\")\n\
        }\n";
    const MAIN: &str = "import lib.H\n\
        fun box(): String {\n\
        \x20 val h = H()\n\
        \x20 val ok = h.size(listOf(\"a\", \"b\", \"c\")) == 3 && h.join(listOf(\"x\", \"y\")) == \"x,y\"\n\
        \x20 return if (ok) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run_with_lib("z1_str", LIB, MAIN).expect("List<String> param"),
        "OK"
    );
}

#[test]
fn method_with_list_int_param() {
    // A primitive-element `List<Int>` parameter — the element still erases, the interface bridges.
    const LIB: &str = "package lib\nclass H { fun total(xs: List<Int>): Int = xs.sum() }\n";
    const MAIN: &str = "import lib.H\n\
        fun box(): String = if (H().total(listOf(1, 2, 3)) == 6) \"OK\" else \"fail\"\n";
    assert_eq!(
        run_with_lib("z1_int", LIB, MAIN).expect("List<Int> param"),
        "OK"
    );
}

#[test]
fn method_with_custom_element_list_param() {
    // A List of a NON-stdlib element type (`List<Info>`) as a parameter — the element identity is
    // irrelevant once erased; the interface match is what matters.
    const LIB: &str = "package lib\n\
        data class Info(val n: Int)\n\
        class Port { fun total(xs: List<Info>): Int = xs.sumOf { it.n } }\n";
    const MAIN: &str = "import lib.Port\nimport lib.Info\n\
        fun box(): String =\n\
        \x20 if (Port().total(listOf(Info(2), Info(3))) == 5) \"OK\" else \"fail\"\n";
    assert_eq!(
        run_with_lib("z1_custom", LIB, MAIN).expect("List<Info> param"),
        "OK"
    );
}

#[test]
fn nonsuspend_returns_custom_list_via_list_param() {
    // The non-suspend shape of w1: `get(ids: List<Int>): List<Info>` — param AND return are collections.
    const LIB: &str = "package lib\n\
        data class Info(val n: Int)\n\
        class Port { fun get(ids: List<Int>): List<Info> = ids.map { Info(it) } }\n";
    const MAIN: &str = "import lib.Port\n\
        fun box(): String {\n\
        \x20 val xs = Port().get(listOf(1, 2, 3))\n\
        \x20 return if (xs.sumOf { it.n } == 6) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run_with_lib("w1_np", LIB, MAIN).expect("List param + custom List return"),
        "OK"
    );
}

#[test]
fn suspend_interface_get_with_list_param_lowers() {
    // w1 exactly: a `suspend` interface member `get(ids: List<Int>): List<Info>`. The `List<Int>` PARAM was
    // the resolution failure (`unresolved method 'get' on 'lib/Port'`), not the return. Asserts the caller
    // LOWERS end-to-end (a coroutine RUN needs a driver, out of scope); the library is built by real kotlinc.
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(libout) = common::compile_lib(
        "w1_susp",
        "package lib\n\
         data class Info(val n: Int)\n\
         interface Port { suspend fun get(ids: List<Int>): List<Info> }\n",
    ) else {
        return;
    };
    let cp: Vec<PathBuf> = vec![libout, sl];
    const MAIN: &str = "import lib.Port\n\
        suspend fun use(p: Port): Int = p.get(listOf(1, 2)).sumOf { it.n }\n\
        fun box(): String = \"OK\"\n";
    assert_eq!(
        common::compile_and_run_box(MAIN, "Main", &cp, Some(&jdk))
            .expect("suspend List-param lowers"),
        "OK"
    );
}

// --- The RETURN-focused w1 shape (`suspend fun …(): List<CustomType>`) with NO collection PARAMETER ---
// The build.663 report framed w1 as "suspend interface method RETURNING List<CustomType>". Investigation
// showed the return was never the failure (the suspend collection RETURN was already recovered — see
// suspend_collection_hof_e2e); the `unresolved method 'get' on 'lib/Port'` came solely from a collection
// PARAMETER, the same root as z1. These lock that a return-only suspend member (a no-arg one, and one with a
// scalar `Int` arg — neither has a collection param) resolves AND lowers, so a regression in the collection-
// param pass can never silently break the return shape either. A coroutine RUN needs a driver (out of scope);
// LOWERING is the bar, exactly as `suspend_interface_get_with_list_param_lowers`.

fn suspend_lowers(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let libout = common::compile_lib(tag, lib)?;
    let cp: Vec<PathBuf> = vec![libout, sl];
    common::compile_and_run_box(main, "Main", &cp, Some(&jdk))
}

#[test]
fn suspend_interface_noarg_returns_custom_list_lowers() {
    // `suspend fun all(): List<Info>` — no parameter at all; the custom-element List return resolves + lowers.
    const LIB: &str = "package lib\n\
        data class Info(val n: Int)\n\
        interface Port { suspend fun all(): List<Info> }\n";
    const MAIN: &str = "import lib.Port\n\
        suspend fun use(p: Port): Int = p.all().size\n\
        fun box(): String = \"OK\"\n";
    assert_eq!(
        suspend_lowers("w1_noarg", LIB, MAIN).expect("no-arg suspend List return lowers"),
        "OK"
    );
}

#[test]
fn suspend_interface_scalar_arg_returns_custom_list_lowers() {
    // `suspend fun get(id: Int): List<Info>` — a SCALAR param (no collection), the literal return-focused w1
    // reproducer; resolves + lowers (was never the actual failure, unlike the collection-param shape above).
    const LIB: &str = "package lib\n\
        data class Info(val n: Int)\n\
        interface Port { suspend fun get(id: Int): List<Info> }\n";
    const MAIN: &str = "import lib.Port\n\
        suspend fun use(p: Port): Int = p.get(1).size\n\
        fun box(): String = \"OK\"\n";
    assert_eq!(
        suspend_lowers("w1_scalar", LIB, MAIN).expect("scalar-arg suspend List return lowers"),
        "OK"
    );
}

#[test]
fn method_with_set_param() {
    // The fix generalizes to any collection: `Set<String>` → `Ljava/util/Set;`.
    const LIB: &str = "package lib\nclass H { fun count(s: Set<String>): Int = s.size }\n";
    const MAIN: &str = "import lib.H\n\
        fun box(): String = if (H().count(setOf(\"a\", \"b\")) == 2) \"OK\" else \"fail\"\n";
    assert_eq!(run_with_lib("set_p", LIB, MAIN).expect("Set param"), "OK");
}

#[test]
fn method_with_map_param() {
    // `Map<String, Int>` → `Ljava/util/Map;`.
    const LIB: &str = "package lib\nclass H { fun keys(m: Map<String, Int>): Int = m.size }\n";
    const MAIN: &str = "import lib.H\n\
        fun box(): String = if (H().keys(mapOf(\"a\" to 1)) == 1) \"OK\" else \"fail\"\n";
    assert_eq!(run_with_lib("map_p", LIB, MAIN).expect("Map param"), "OK");
}

#[test]
fn method_with_mutable_list_param() {
    // The mutable collection erases to the SAME JVM interface (`MutableList<Int>` → `Ljava/util/List;`).
    const LIB: &str = "package lib\nclass H { fun n(xs: MutableList<Int>): Int = xs.size }\n";
    const MAIN: &str = "import lib.H\n\
        fun box(): String {\n\
        \x20 val m = mutableListOf(1, 2, 3)\n\
        \x20 return if (H().n(m) == 3) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run_with_lib("mutlist_p", LIB, MAIN).expect("MutableList param"),
        "OK"
    );
}

#[test]
fn overloads_by_collection_interface_stay_distinct() {
    // SOUNDNESS: the JVM-descriptor-form pass must not conflate distinct collection interfaces. A class with
    // both `f(List<Int>)` and `f(Set<Int>)` — a `listOf` call selects the `List` overload, a `setOf` call the
    // `Set` one (`java/util/List` != `java/util/Set`), rather than the pass grabbing whichever comes first.
    const LIB: &str = "package lib\n\
        class H {\n\
        \x20 fun f(xs: List<Int>): String = \"list\"\n\
        \x20 fun f(xs: Set<Int>): String = \"set\"\n\
        }\n";
    const MAIN: &str = "import lib.H\n\
        fun box(): String {\n\
        \x20 val h = H()\n\
        \x20 return if (h.f(listOf(1)) == \"list\" && h.f(setOf(1)) == \"set\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run_with_lib("ovl_distinct", LIB, MAIN).expect("List vs Set overloads distinct"),
        "OK"
    );
}
