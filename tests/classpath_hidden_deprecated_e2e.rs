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

const HIDDEN_OPEN: &str = "package lib\n\
    open class Base {\n\
    \x20   @Deprecated(\"use the named form\", level = DeprecationLevel.HIDDEN)\n\
    \x20   open fun limited(n: Int): Int = n\n\
    \x20   open fun limited(n: Int, name: String? = null): Int = n + (name?.length ?: 0)\n\
    \x20   @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
    \x20   open val value: String = \"base\"\n\
    \x20   @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
    \x20   open var count: Int = 0\n\
    }\n";

/// kotlinc keeps a HIDDEN member out of calls and still accepts `override` of that source arity.
/// The hidden JVM method is synthetic, so Java source cannot name it; reflection dispatches
/// through the override and must not run the hidden body.
#[test]
fn hidden_open_member_remains_overridable() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let lib =
        common::compile_lib("hidden-override", HIDDEN_OPEN).expect("hidden open member library");
    let main = "import lib.Base\n\
        class Child : Base() {\n\
        \x20   override fun limited(n: Int): Int = n + 1\n\
        \x20   override val value: String = \"child\"\n\
        \x20   override var count: Int = 7\n\
        }\n\
        fun box(): String {\n\
        \x20   val method = Base::class.java.getDeclaredMethod(\"limited\", Int::class.javaPrimitiveType)\n\
        \x20   val called = method.invoke(Child(), 6) as Int\n\
        \x20   val visible = Child().limited(1, null)\n\
        \x20   val child = Child()\n\
        \x20   val value = Base::class.java.getDeclaredMethod(\"getValue\").invoke(child) as String\n\
        \x20   Base::class.java.getDeclaredMethod(\"setCount\", Int::class.javaPrimitiveType).invoke(child, 9)\n\
        \x20   val count = Base::class.java.getDeclaredMethod(\"getCount\").invoke(child) as Int\n\
        \x20   return if (called == 7 && visible == 1 && value == \"child\" && count == 9) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::compile_and_run_box(main, "Child", &[lib, stdlib], Some(jdk.as_path())).as_deref(),
        Some("OK")
    );
}

#[test]
fn hidden_property_is_not_a_read_candidate() {
    let lib =
        common::compile_lib("hidden-property-read", HIDDEN_OPEN).expect("hidden property library");
    let consumer = "import lib.Base\n\
        fun read(base: Base): String = base.value\n";
    let result =
        common::compiler_diagnostics(&[("Read.kt", consumer)], &[lib, common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "hidden property read");
}

/// A shorter prefix of a function with a default is not an override unless that prefix is itself
/// a HIDDEN declaration. A classpath HIDDEN member that is final by Kotlin's default is not
/// overridable.
#[test]
fn default_argument_prefix_and_hidden_final_override_nothing() {
    let classpath = [common::stdlib_jar()];
    let prefix = "open class Base {\n\
        \x20   open fun f(a: Int, b: Int = 0): Int = a + b\n\
        }\n\
        class Child : Base() {\n\
        \x20   override fun f(a: Int): Int = a\n\
        }\n";
    let prefix_result = common::compiler_diagnostics(&[("Prefix.kt", prefix)], &classpath);
    common::expect_identical_rejection(&prefix_result, "default argument prefix");
    let lib = "package lib\n\
        open class Base {\n\
        \x20   @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
        \x20   fun fixed(n: Int): Int = n\n\
        \x20   @Deprecated(\"gone\", level = DeprecationLevel.HIDDEN)\n\
        \x20   val fixedValue: String = \"base\"\n\
        }\n";
    let lib = common::compile_lib("hidden-final", lib).expect("hidden final member library");
    let consumer = "import lib.Base\n\
        class Child : Base() {\n\
        \x20   override fun fixed(n: Int): Int = n + 1\n\
        \x20   override val fixedValue: String = \"child\"\n\
        }\n";
    let final_result =
        common::compiler_diagnostics(&[("Child.kt", consumer)], &[lib, common::stdlib_jar()]);
    common::expect_identical_rejection(&final_result, "hidden final member");
}

/// A HIDDEN declaration participates in override matching with its complete signature. A sibling
/// of the same source arity but a different parameter type cannot make an unrelated override legal.
#[test]
fn hidden_same_arity_incompatible_signature_overrides_nothing() {
    let lib =
        common::compile_lib("hidden-override-type", HIDDEN_OPEN).expect("hidden signature library");
    let consumer = "import lib.Base\n\
        class Child : Base() {\n\
        \x20   override fun limited(n: String): Int = n.length\n\
        }\n";
    let result =
        common::compiler_diagnostics(&[("Child.kt", consumer)], &[lib, common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "hidden incompatible same-arity override");
}

const HIDDEN_GENERIC: &str = "package lib\n\
    open class GenericBase<T> {\n\
    \x20   @Deprecated(\"hidden\", level = DeprecationLevel.HIDDEN)\n\
    \x20   open fun convert(value: T): String = \"base\"\n\
    }\n\
    open class GenericMid<U> : GenericBase<List<U>>()\n";

/// The common hierarchy specializes the retained declaration before matching the override. The
/// generated bridge must still dispatch a call through the erased hidden base method.
#[test]
fn inherited_hidden_generic_signature_is_specialized() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let lib = common::compile_lib("hidden-override-generic", HIDDEN_GENERIC)
        .expect("hidden generic library");
    let main = "import lib.GenericBase\n\
        import lib.GenericMid\n\
        class Child : GenericMid<String>() {\n\
        \x20   override fun convert(value: List<String>): String = value.single()\n\
        }\n\
        fun box(): String {\n\
        \x20   val method = GenericBase::class.java.getDeclaredMethod(\"convert\", Object::class.java)\n\
        \x20   return method.invoke(Child(), listOf(\"OK\")) as String\n\
        }\n";
    assert_eq!(
        common::compile_and_run_box(main, "Child", &[lib, stdlib], Some(jdk.as_path())).as_deref(),
        Some("OK")
    );
}
