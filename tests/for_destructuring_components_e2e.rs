//! `for`-loop destructuring through member extensions and `withIndex()`.

use super::common;

/// Strict stdlib/JDK run: missing tooling or a rejected source panics with diagnostics.
fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "Main")
}

#[test]
fn member_extension_componentn_in_for_destructuring() {
    const SRC: &str = "class C(val i: Int)\n\
class M {\n\
    operator fun C.component1() = i + 1\n\
    operator fun C.component2() = i + 2\n\
    fun doTest(l: Array<C>): String {\n\
        var s = \"\"\n\
        for ((a, b) in l) {\n\
            s += \"$a:$b;\"\n\
        }\n\
        return s\n\
    }\n\
}\n\
fun box(): String {\n\
    val l = Array<C>(3) { x -> C(x) }\n\
    val s = M().doTest(l)\n\
    return if (s == \"1:2;2:3;3:4;\") \"OK\" else \"fail: $s\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn member_extension_precedes_top_level_extension() {
    const SRC: &str = "class C\n\
operator fun C.component1() = \"top\"\n\
class M {\n\
    operator fun C.component1() = \"member\"\n\
    fun result(): String {\n\
        val (value) = C()\n\
        return value\n\
    }\n\
}\n\
fun box(): String = if (M().result() == \"member\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn enclosing_member_extension_is_available_in_inner_class() {
    const SRC: &str = "class C(val value: String)\n\
class Outer {\n\
    operator fun C.component1() = value\n\
    inner class Inner {\n\
        fun result(): String {\n\
            val (value) = C(\"OK\")\n\
            return value\n\
        }\n\
    }\n\
}\n\
fun box(): String = Outer().Inner().result()\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn component_member_extension_requires_operator() {
    const SRC: &str = "class C\n\
class M {\n\
    fun C.component1() = 1\n\
    fun test() {\n\
        val (value) = C()\n\
    }\n\
}\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("'operator' modifier is required")),
        "{diagnostics:?}"
    );
}

#[test]
fn ordinary_component_member_requires_operator() {
    const SRC: &str = "class C { fun component1() = 1 }\n\
fun test() { val (value) = C() }\n";
    let diagnostics = common::front_end_diagnostics(SRC, &[], None);
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("'operator' modifier is required")),
        "{diagnostics:?}"
    );
}

#[test]
fn internal_component_extension_is_visible_across_module_files() {
    let sources = [
        (
            "Components",
            "class Box(val value: String)\n\
internal operator fun Box.component1(): String = value\n",
        ),
        (
            "Main",
            "fun box(): String { val (value) = Box(\"OK\"); return value }\n",
        ),
    ];
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
}

#[test]
fn bracket_destructuring_over_with_index() {
    const SRC: &str =
        "// LANGUAGE: +NameBasedDestructuring +EnableNameBasedDestructuringShortForm\n\
fun box(): String {\n\
    val s = StringBuilder()\n\
    for ([index, x] in listOf(\"a\", \"b\").withIndex()) {\n\
        s.append(\"$index:$x;\")\n\
    }\n\
    val ss = s.toString()\n\
    return if (ss == \"0:a;1:b;\") \"OK\" else \"fail: $ss\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn unsigned_with_index_destructures_to_value_type() {
    const SRC: &str = "fun box(): String {\n\
    var sum = 0\n\
    for ((index, element) in (1u..5u).withIndex()) {\n\
        sum = sum * 10 + index * element.toInt()\n\
    }\n\
    return if (sum == 2740) \"OK\" else \"fail: $sum\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn with_index_over_custom_iterable_destructures() {
    const SRC: &str = "class Wrap<out T>(private val s: Iterable<T>) : Iterable<T> {\n\
    override fun iterator() = s.iterator()\n\
}\n\
fun plusHalf(value: Double): Double = value + 0.5\n\
fun box(): String {\n\
    val xs = Wrap(listOf(1.0, 2.0))\n\
    var total = 0.0\n\
    for ((index, value) in xs.withIndex()) {\n\
        total += plusHalf(value) + index\n\
    }\n\
    return if (total == 5.0) \"OK\" else \"fail: $total\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn charsequence_impl_length_stub_dispatches() {
    const SRC: &str =
        "class Chars(private val s: String, override val length: Int = s.length) : CharSequence {\n\
    override fun get(index: Int): Char = s[index]\n\
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
        s.subSequence(startIndex, endIndex)\n\
}\n\
fun box(): String {\n\
    val cs = Chars(\"ab\")\n\
    val sb = StringBuilder()\n\
    for ((i, c) in cs.withIndex()) {\n\
        sb.append(\"$i:$c;\")\n\
    }\n\
    val ss = sb.toString()\n\
    return if (ss == \"0:a;1:b;\") \"OK\" else \"fail: $ss\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn inherited_charsequence_members_get_mapped_bridges() {
    const SRC: &str = "open class CharsBase(protected val s: String) {\n\
    val length: Int get() = s.length\n\
    operator fun get(index: Int): Char = s[index]\n\
    fun subSequence(startIndex: Int, endIndex: Int): CharSequence =\n\
        s.subSequence(startIndex, endIndex)\n\
}\n\
class Chars(s: String) : CharsBase(s), CharSequence\n\
fun box(): String {\n\
    val value: CharSequence = Chars(\"OK\")\n\
    return \"${value[0]}${value[1]}:${value.length}\"\n\
}\n";
    assert_eq!(run(SRC), "OK:2");
}

#[test]
fn for_destructuring_over_with_index() {
    const SRC: &str = "fun box(): String {\n\
    val s = StringBuilder()\n\
    for ((index, x) in listOf(\"a\", \"b\").withIndex()) {\n\
        s.append(\"$index:$x;\")\n\
    }\n\
    val ss = s.toString()\n\
    return if (ss == \"0:a;1:b;\") \"OK\" else \"fail: $ss\"\n\
}\n";
    assert_eq!(run(SRC), "OK");
}
