//! `@JvmStatic` on source functions of named objects and companions.
//!
//! A named object's member is a `static` method of the object, which every call `invokestatic`s
//! and whose body reads `INSTANCE` for its receiver. A companion's member stays an instance method
//! that calls keep reaching through the companion, while the companion's classifier gains a static
//! bridge carrying the member's annotations — the method a framework such as a test runner finds.

use super::common;

const SINGLE_FILE: &str = "annotation class BeforeAll\n\
annotation class Tag(val value: String)\n\
\n\
class Suite {\n\
    companion object {\n\
        private val prefix = \"s\"\n\
        @JvmStatic @BeforeAll fun setup(name: String, count: Int): String = prefix + name + count\n\
        @JvmStatic @Tag(\"t\") fun <T> echo(value: T?): T? = value\n\
        @JvmStatic fun touch(note: String?) {\n\
            println(note)\n\
        }\n\
        fun plain(): Int = 3\n\
    }\n\
    fun viaInstance(): String = setup(\"i\", 1)\n\
}\n\
\n\
object Registry {\n\
    val base = 5\n\
    @JvmStatic fun offset(x: Int): Int = x + base + helper()\n\
    fun helper(): Int = 1\n\
    @JvmStatic fun describe(label: String): String = label + this.base\n\
    @JvmStatic fun padded(a: Int, b: Int = 4): Int = a + b\n\
    fun callsStatic(): Int = offset(1) + padded(1)\n\
}\n\
\n\
fun box(): String {\n\
    val registry = Registry\n\
    val total = Registry.offset(1) + registry.offset(2) + Registry.padded(1) + Registry.padded(1, 2) +\n\
        Registry.callsStatic()\n\
    Suite.touch(null)\n\
    val text = Suite.setup(\"x\", 2) + Suite.echo(\"e\") + Suite().viaInstance() + Registry.describe(\"d\")\n\
    return if (total == 35 && text == \"sx2esi1d5\") \"OK\" else \"fail $total $text\"\n\
}\n";

const LIBRARY_FILE: &str = "object Counter {\n\
    val start = 10\n\
    @JvmStatic fun next(step: Int): Int = start + step\n\
    @JvmStatic fun scaled(value: Int, factor: Int = 3): Int = value * factor\n\
}\n\
\n\
class Host {\n\
    companion object {\n\
        @JvmStatic fun greet(name: String): String = \"hi \" + name\n\
    }\n\
}\n";

const CALLER_FILE: &str = "fun box(): String {\n\
    val counter = Counter\n\
    val sum = Counter.next(1) + counter.next(2) + Counter.scaled(2) + Counter.scaled(2, 4)\n\
    val text = Host.greet(\"x\")\n\
    return if (sum == 37 && text == \"hi x\") \"OK\" else \"fail $sum $text\"\n\
}\n";

const OTHER_OWNERS: &str = "interface Face {\n\
    companion object {\n\
        @JvmStatic fun create(): String = \"face\"\n\
    }\n\
}\n\
\n\
enum class Mode {\n\
    A, B;\n\
    companion object {\n\
        @JvmStatic fun first(): Mode = A\n\
    }\n\
}\n\
\n\
class Defaults(val x: Int = 1) {\n\
    companion object {\n\
        @JvmStatic fun make(seed: Int = 2): Defaults = Defaults(seed)\n\
        @JvmStatic fun sum(vararg values: Int): Int = values.sum()\n\
    }\n\
}\n\
\n\
fun box(): String {\n\
    val ok = Face.create() == \"face\" && Mode.first() == Mode.A && Defaults.make().x == 2 &&\n\
        Defaults.sum(1, 2, 3) == 6\n\
    return if (ok) \"OK\" else \"fail\"\n\
}\n";

/// Private statics: a nested class reaches the object's through `access$<name>`, and the companion
/// bridge of a private member reaches it through the companion's accessor.
const PRIVATE_MEMBERS: &str = "object Vault {\n\
    @JvmStatic private fun secret(): Int = 7\n\
    @JvmStatic fun open(): Int = 8\n\
    fun reveal(): Int = secret()\n\
    class Peek { fun look(): Int = secret() }\n\
}\n\
\n\
class Keeper {\n\
    companion object {\n\
        @JvmStatic private fun hidden(): Int = 1\n\
        fun useHidden(): Int = hidden()\n\
        @JvmStatic internal fun inner(): Int = 2\n\
    }\n\
}\n\
\n\
fun box(): String {\n\
    val sum = Vault.reveal() + Vault.open() + Vault.Peek().look() + Keeper.useHidden() + Keeper.inner()\n\
    return if (sum == 25) \"OK\" else \"fail $sum\"\n\
}\n";

/// Java reflection sees what a Java caller or a test framework sees: real statics, and the
/// companion bridge's copied annotations.
const REFLECTED: &str = "import java.lang.reflect.Modifier\n\
\n\
annotation class BeforeAll\n\
\n\
class Suite {\n\
    companion object {\n\
        var ran = 0\n\
        @JvmStatic @BeforeAll fun setup() { ran++ }\n\
    }\n\
}\n\
\n\
object Registry {\n\
    @JvmStatic fun twice(x: Int): Int = x * 2\n\
}\n\
\n\
fun box(): String {\n\
    val bridge = Suite::class.java.getMethod(\"setup\")\n\
    if (!Modifier.isStatic(bridge.modifiers)) return \"bridge is not static\"\n\
    if (!bridge.isAnnotationPresent(BeforeAll::class.java)) return \"bridge lost its annotation\"\n\
    if (!bridge.isAnnotationPresent(JvmStatic::class.java)) return \"bridge lost @JvmStatic\"\n\
    bridge.invoke(null)\n\
    if (Suite.ran != 1) return \"bridge did not reach the companion\"\n\
    val member = Suite.Companion::class.java.getMethod(\"setup\")\n\
    if (Modifier.isStatic(member.modifiers)) return \"companion member became static\"\n\
    val twice = Registry::class.java.getMethod(\"twice\", Int::class.javaPrimitiveType)\n\
    if (!Modifier.isStatic(twice.modifiers)) return \"object member is not static\"\n\
    if (twice.invoke(null, 21) != 42) return \"object static returned the wrong value\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn object_statics_and_companion_bridges_match_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Static.kt", SINGLE_FILE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn object_statics_and_companion_bridges_run() {
    common::expect_box_ok_with_stdlib(SINGLE_FILE, "jvm static members");
}

#[test]
fn a_sibling_file_calls_object_statics_like_kotlinc() {
    let sources = [("Lib.kt", LIBRARY_FILE), ("Main.kt", CALLER_FILE)];
    let classes = common::classes_against_kotlinc_module(&sources);
    assert_eq!(classes.differences(), Vec::<String>::new());
    common::expect_box_ok_files_with_stdlib(&sources, "sibling-file jvm static calls");
}

#[test]
fn interface_enum_and_defaulted_companions_get_bridges_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Shapes.kt", OTHER_OWNERS)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
    common::expect_box_ok_with_stdlib(OTHER_OWNERS, "jvm static bridge owners");
}

#[test]
fn reflection_finds_the_statics_and_the_annotated_bridge() {
    common::expect_box_ok_with_stdlib(REFLECTED, "jvm static reflection");
}

#[test]
fn private_statics_and_their_accessors_match_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Private.kt", PRIVATE_MEMBERS)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
    common::expect_box_ok_with_stdlib(PRIVATE_MEMBERS, "private jvm static members");
}
