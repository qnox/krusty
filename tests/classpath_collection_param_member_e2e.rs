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
use std::path::PathBuf;
mod common;

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
