//! `@Deprecated(level = HIDDEN)` classpath callables are invisible to overload resolution.
//!
//! kotlinc removes HIDDEN-deprecated declarations from the candidate set entirely (they exist
//! only for binary compatibility; the JVM realization is emitted `ACC_SYNTHETIC`). krusty kept
//! them, so a library that hides a superseded constructor overload — kotlinpoet's
//! `ClassName(String, String, vararg String)` — made every call ambiguous and reported
//! "unresolved function" where kotlinc resolves the one visible candidate.
//!
//! The library is krusty-compiled: this is an end-to-end producer/consumer regression proving
//! argument-bearing method annotations survive Krusty emission and become classpath selection facts.
use super::common;

const LIB: &str = "package lib\n\
    class Handle internal constructor(val names: List<String>) {\n\
    \x20   @Deprecated(\"use the two-part form\", level = DeprecationLevel.HIDDEN)\n\
    \x20   constructor(packageName: String, simpleName: String, vararg simpleNames: String) :\n\
    \x20       this(listOf(packageName, simpleName) + simpleNames)\n\
    \x20   constructor(packageName: String, vararg simpleNames: String) :\n\
    \x20       this(listOf(packageName) + simpleNames)\n\
    \x20   val display: String get() = names.joinToString(\".\")\n\
    }\n\
    @Deprecated(\"use the Any form\", level = DeprecationLevel.HIDDEN)\n\
    fun pick(value: String): String = \"hidden\"\n\
    fun pick(value: Any): String = \"visible\"\n\
    class Box(val tag: String) {\n\
    \x20   @Deprecated(\"use the Any form\", level = DeprecationLevel.HIDDEN)\n\
    \x20   fun label(value: String): String = \"hidden\"\n\
    \x20   fun label(value: Any): String = \"visible:$tag\"\n\
    }\n\
    @Deprecated(\"use the Any form\", level = DeprecationLevel.HIDDEN)\n\
    fun String.mark(): String = \"hidden\"\n\
    fun Any.mark(): String = \"visible\"\n\
    object Registry {\n\
    \x20   @Deprecated(\"use the Any form\", level = DeprecationLevel.HIDDEN)\n\
    \x20   fun of(value: String): String = \"hidden\"\n\
    \x20   fun of(value: Any): String = \"visible\"\n\
    }\n";

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let libout = common::compile_lib("hd1", LIB)?;
    common::compile_and_run_box(main, "Main", &[libout, sl], Some(jdk.as_path()))
}

#[test]
fn hidden_constructor_leaves_visible_overload_unambiguous() {
    // Without the filter both the hidden `(String, String, vararg String)` and the visible
    // `(String, vararg String)` admit two String arguments and the call dies as unresolved.
    const MAIN: &str = "import lib.Handle\n\
        fun box(): String {\n\
        \x20   val h = Handle(\"com.example\", \"Foo\")\n\
        \x20   return if (h.display == \"com.example.Foo\") \"OK\" else \"fail:\" + h.display\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("hidden ctor overload must not block resolution"),
        "OK"
    );
}

#[test]
fn hidden_top_level_function_is_not_a_candidate() {
    // The hidden `pick(String)` is MORE specific than the visible `pick(Any)`; keeping it would
    // silently select the hidden body ("hidden") instead of kotlinc's answer ("visible").
    const MAIN: &str = "import lib.pick\n\
        fun box(): String {\n\
        \x20   val p = pick(\"x\")\n\
        \x20   return if (p == \"visible\") \"OK\" else \"fail:\" + p\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("hidden top-level fn must not win selection"),
        "OK"
    );
}

#[test]
fn hidden_member_function_is_not_a_candidate() {
    const MAIN: &str = "import lib.Box\n\
        fun box(): String {\n\
        \x20   val l = Box(\"t\").label(\"x\")\n\
        \x20   return if (l == \"visible:t\") \"OK\" else \"fail:\" + l\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("hidden member fn must not win selection"),
        "OK"
    );
}

#[test]
fn hidden_extension_is_not_a_candidate() {
    // The package-facade extension channel: the hidden `String.mark()` is more specific than the
    // visible `Any.mark()` and would win receiver ranking if it stayed a candidate.
    const MAIN: &str = "import lib.mark\n\
        fun box(): String {\n\
        \x20   val m = \"x\".mark()\n\
        \x20   return if (m == \"visible\") \"OK\" else \"fail:\" + m\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("hidden extension must not win selection"),
        "OK"
    );
}

#[test]
fn hidden_imported_object_member_is_not_a_candidate() {
    // The import-into-scope channel (`import lib.Registry.of`) surfaces object members through
    // `object_member_callables`, a separate path from qualified `Registry.of(...)` selection.
    const MAIN: &str = "import lib.Registry.of\n\
        fun box(): String {\n\
        \x20   val v = of(\"x\")\n\
        \x20   return if (v == \"visible\") \"OK\" else \"fail:\" + v\n\
        }\n";
    assert_eq!(
        run(MAIN).expect("hidden imported object member must not win selection"),
        "OK"
    );
}
