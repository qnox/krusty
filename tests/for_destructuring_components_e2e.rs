//! `for`-loop destructuring shapes: componentN as a MEMBER EXTENSION (`operator fun
//! C.component1()` declared inside the enclosing class) and the bracket short form over
//! `withIndex()`. Mirrors corpus `arrays/multiDecl/MultiDeclForComponentMemberExtensions.kt` and
//! `controlStructures/forInIterableWithIndex/*`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
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
    assert_eq!(run(SRC).expect("member-ext componentN destructure"), "OK");
}

#[test]
fn bracket_destructuring_over_with_index() {
    // The corpus `forInIterableWithIndex` shape: the name-based short form over `withIndex()`.
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
    if let Some(out) = run(SRC) {
        assert_eq!(out, "OK");
    } else {
        panic!("bracket withIndex destructure did not compile");
    }
}

#[test]
fn with_index_over_custom_iterable_destructures() {
    // The corpus `forInIterableWithIndexCheckSideEffects` shape: `withIndex()` on a USER class
    // implementing `Iterable<T>` — the element type flows through the supertype's type argument
    // into `IndexedValue<T>`, so the destructured `x` keeps `T`.
    const SRC: &str = "class Wrap<out T>(private val s: Iterable<T>) : Iterable<T> {\n\
    override fun iterator() = s.iterator()\n\
}\n\
fun box(): String {\n\
    val xs = Wrap(listOf(\"a\", \"b\"))\n\
    val s = StringBuilder()\n\
    for ((index, x) in xs.withIndex()) {\n\
        s.append(\"$index:$x;\")\n\
    }\n\
    val ss = s.toString()\n\
    return if (ss == \"0:a;1:b;\") \"OK\" else \"fail: $ss\"\n\
}\n";
    assert_eq!(run(SRC).expect("custom-iterable withIndex"), "OK");
}

#[test]
fn charsequence_impl_length_stub_dispatches() {
    // A user `CharSequence` implementor's `length` PROPERTY must also provide the JVM `length()`
    // method (kotlinc's built-in java-mapping) — interface dispatch (here through `withIndex`'s
    // iterator) otherwise throws `AbstractMethodError`. Mirrors corpus
    // `forInCharSequenceWithIndex/*` / `forInCharSequenceWithMultipleGetFunctions.kt`.
    const SRC: &str = "class Chars(private val s: String) : CharSequence {\n\
    override val length: Int\n\
        get() = s.length\n\
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
    assert_eq!(run(SRC).expect("CharSequence length stub"), "OK");
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
    assert_eq!(run(SRC).expect("withIndex destructure"), "OK");
}
