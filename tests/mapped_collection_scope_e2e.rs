//! A mapped Kotlin collection's member scope is its `.kotlin_builtins` declaration plus kotlinc's
//! explicit JDK-member whitelist (`JvmBuiltInsSignatures.VISIBLE_METHOD_SIGNATURES`), not the whole
//! method set of the `java.util` class it maps to. `java.util.List` declares `remove(int)` (remove BY
//! INDEX) alongside `remove(Object)` (remove the ELEMENT); Kotlin's `MutableList` declares only
//! `MutableCollection.remove(element: E)`, with the index-taking method reachable solely as
//! `removeAt`. Taking the Java set MISCOMPILED `list.remove(10)` — an `Int` argument fits the
//! primitive `int` parameter exactly, so it won overload resolution and removed whichever element sat
//! at index 10. Dropping the whitelist instead under-resolves the JDK default methods kotlinc keeps
//! (`MutableMap.computeIfAbsent`, `List.stream`, …) — mutating ones on the mutable face only.
//!
//! Two receiver shapes, two mechanisms, mirroring kotlinc. A MAPPED name resolves against the builtins
//! declaration directly. A real JVM class in the hierarchy (`java.util.ArrayList`) keeps its Java scope,
//! with a method that a renamed builtin covers exposed under the Kotlin name instead of its own.

use super::common;

#[test]
fn remove_on_a_mapped_receiver_takes_the_element_overload() {
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        fun box(): String {
            val l: MutableList<Int> = mutableListOf(10, 20, 30)
            val removed = l.remove(10)
            return "$removed $l"
        }
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "true [20, 30]");
}

#[test]
fn remove_of_an_absent_element_is_not_an_index() {
    // `remove(10)` on a three-element list would be an out-of-range INDEX; as an ELEMENT it is simply
    // absent, so the call returns `false` and leaves the list alone.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        fun box(): String {
            val l: MutableList<Int> = mutableListOf(1, 2, 3)
            val removed = l.remove(10)
            return "$removed $l"
        }
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "false [1, 2, 3]");
}

#[test]
fn remove_on_a_concrete_java_class_takes_the_element_overload() {
    // `java.util.ArrayList` is a real classfile, so it never consults the builtins — it declares its own
    // `remove(int)`. The Java member scope must rename that one out of `remove`, or this silently throws.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        fun box(): String {
            val a: ArrayList<Int> = arrayListOf(10, 20, 30)
            val removed = a.remove(10)
            return "$removed $a"
        }
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "true [20, 30]");
}

#[test]
fn same_shaped_java_method_outside_the_collection_hierarchy_keeps_its_name() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    // Descriptor equality alone is insufficient: this method deliberately has the same
    // `(I)Object` shape as `java.util.List.remove(int)`, but its class does not realize the mapped
    // `MutableList.removeAt` obligation. The provider-derived rename must therefore leave `remove`
    // visible and must not invent a `removeAt` member.
    let java = [(
        "Unrelated.java".to_string(),
        r#"
            package fixtures;
            public final class Unrelated {
                public Object remove(int index) { return index == 7 ? "OK" : "fail"; }
            }
        "#
        .to_string(),
    )];
    let Some((classes, _)) = common::javac_compile(&java, &[]) else {
        return;
    };
    let root = classes.parent().map(std::path::Path::to_path_buf);
    let classpath = vec![classes, stdlib];
    let output = common::compile_and_run_box(
        "import fixtures.Unrelated\nfun box(): String = Unrelated().remove(7).toString()\n",
        "Main",
        &classpath,
        Some(&jdk),
    );
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    assert_eq!(output.as_deref(), Some("OK"));
}

#[test]
fn remove_at_reaches_the_index_overload_on_both_shapes() {
    // The renamed member stays reachable under its Kotlin name on either receiver, and still emits
    // `remove(I)`.
    let Some(output) = common::compile_and_run_with_stdlib(
        r#"
        fun box(): String {
            val l: MutableList<Int> = mutableListOf(10, 20, 30)
            val first = l.removeAt(0)
            val a: ArrayList<String> = arrayListOf("a", "b")
            val last = a.removeAt(1)
            return "$first $l $last $a"
        }
        "#,
        "Main",
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "10 [20, 30] b [a]");
}

#[test]
fn remove_at_override_satisfies_the_java_util_abstract() {
    // The OVERRIDE direction, unchanged by the scope work: a class realizing the interface writes its
    // override under the Kotlin name, so `java.util.List.remove(int)` stays abstract without a bridge —
    // an `AbstractMethodError` at the first call through the interface. The primitive element type also
    // exercises the bridge's boxing (`removeAt(Int): Int` behind the erased `(I)Ljava/lang/Object;`).
    let output = common::expect_box_run_with_stdlib(
        r#"
        class Ints : MutableList<Int> {
            private val b: MutableList<Int> = mutableListOf(1, 2, 3)
            override fun removeAt(index: Int): Int = b.removeAt(index) * 100
            override val size: Int get() = b.size
            override fun get(index: Int): Int = b[index]
            override fun isEmpty(): Boolean = b.isEmpty()
            override fun contains(element: Int): Boolean = b.contains(element)
            override fun containsAll(elements: Collection<Int>): Boolean = b.containsAll(elements)
            override fun indexOf(element: Int): Int = b.indexOf(element)
            override fun lastIndexOf(element: Int): Int = b.lastIndexOf(element)
            override fun add(element: Int): Boolean = b.add(element)
            override fun remove(element: Int): Boolean = b.remove(element)
            override fun addAll(elements: Collection<Int>): Boolean = b.addAll(elements)
            override fun addAll(index: Int, elements: Collection<Int>): Boolean = b.addAll(index, elements)
            override fun removeAll(elements: Collection<Int>): Boolean = b.removeAll(elements)
            override fun retainAll(elements: Collection<Int>): Boolean = b.retainAll(elements)
            override fun clear() { b.clear() }
            override fun set(index: Int, element: Int): Int = b.set(index, element)
            override fun add(index: Int, element: Int) { b.add(index, element) }
            override fun listIterator(): MutableListIterator<Int> = b.listIterator()
            override fun listIterator(index: Int): MutableListIterator<Int> = b.listIterator(index)
            override fun subList(fromIndex: Int, toIndex: Int): MutableList<Int> = b.subList(fromIndex, toIndex)
            override fun iterator(): MutableIterator<Int> = b.iterator()
            override fun toString(): String = b.toString()
        }

        fun box(): String {
            val ints = Ints()
            val direct = ints.removeAt(0)
            val through: MutableList<Int> = ints
            return "$direct ${through.removeAt(0)} $ints"
        }
        "#,
        "Main",
    );
    assert_eq!(output, "100 200 [3]");
}

#[test]
fn java_only_members_are_not_in_the_kotlin_scope() {
    // The members the JVM class declares and the Kotlin API does not. Each of these compiled before the
    // scope came from the builtins declaration; kotlinc reports every one as unresolved.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for (member, source) in [
        ("getFirst", "fun f(l: List<Int>) { l.getFirst() }"),
        ("add", "fun f(l: List<Int>) { l.add(1) }"),
        // Mutating JDK defaults stay hidden on the READ-ONLY face even though the whitelist below
        // re-admits them on the mutable one (kotlinc `MUTABLE_METHOD_SIGNATURES`).
        ("removeIf", "fun f(l: List<Int>) { l.removeIf { true } }"),
        ("replaceAll", "fun f(l: List<Int>) { l.replaceAll { it } }"),
        (
            "computeIfAbsent",
            "fun f(m: Map<String, Int>) { m.computeIfAbsent(\"k\") { 1 } }",
        ),
        (
            "putIfAbsent",
            "fun f(m: Map<String, Int>) { m.putIfAbsent(\"k\", 1) }",
        ),
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains(&format!("unresolved reference '{member}'"))),
            "{member} should not be a member of the Kotlin scope: {diagnostics:?}"
        );
    }
}

// kotlinc re-admits an explicit JDK-member whitelist over the builtins scope of a mapped collection
// (`JvmBuiltInsSignatures.VISIBLE_METHOD_SIGNATURES`): read-only Java defaults (`stream`,
// `spliterator`, `getOrDefault`, `forEach`, …) on both faces, mutating defaults
// (`MUTABLE_METHOD_SIGNATURES`: `putIfAbsent`, `computeIfAbsent`, `merge`, `removeIf`, …) only on
// the `Mutable*` face. Every case below is pinned against kotlinc 2.4.10.

#[test]
fn whitelisted_map_default_methods_run_on_a_mutable_receiver() {
    // Golden output captured from kotlinc 2.4.10 running this exact source. The two-parameter lambda
    // fits only the whitelisted `BiConsumer`/`BiFunction`/`Function` SAM members, and
    // `return@computeIfAbsent` labels the lambda of the Java member.
    let output = common::expect_box_run_with_stdlib(
        r#"
        fun box(): String {
            val m: MutableMap<String, String> = mutableMapOf("a" to "1")
            val out = StringBuilder()
            out.append(m.putIfAbsent("a", "x"))
            out.append('|')
            out.append(m.putIfAbsent("b", "2"))
            out.append('|')
            out.append(m.computeIfAbsent("c") { k -> k.uppercase() })
            out.append('|')
            out.append(m.computeIfPresent("a") { k, v -> v + k })
            out.append('|')
            out.append(m.compute("d") { k, v -> (v ?: "") + k })
            out.append('|')
            out.append(m.merge("e", "3") { a, b -> a + b })
            out.append('|')
            out.append(m.replace("a", "9"))
            out.append('|')
            out.append(m.replace("b", "2", "7"))
            out.append('|')
            out.append(m.computeIfAbsent("z") {
                return@computeIfAbsent "lbl"
            })
            out.append('|')
            m.replaceAll { k, v -> k + v }
            out.append(m.toString())
            return out.toString()
        }
        "#,
        "Main",
    );
    assert_eq!(
        output,
        "1|null|C|1a|d|3|1a|true|lbl|{a=a9, b=b7, c=cC, d=dd, e=e3, z=zlbl}"
    );
}

#[test]
fn whitelisted_read_only_members_resolve_on_read_only_receivers() {
    // kotlinc 2.4.10 accepts every one of these on the read-only face: `stream`/`parallelStream`/
    // `spliterator` (java.util.Collection whitelist), `spliterator`/`forEach` (java.lang.Iterable),
    // `forEachRemaining` (java.util.Iterator), `getOrDefault`/`forEach` (java.util.Map).
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for source in [
        "fun f(l: List<Int>) = l.stream()",
        "fun f(l: List<Int>) = l.parallelStream()",
        "fun f(l: List<Int>) = l.spliterator()",
        "fun f(c: Collection<Int>) = c.stream()",
        "fun f(i: Iterable<Int>) = i.spliterator()",
        "fun f(i: Iterator<Int>) = i.forEachRemaining { println(it) }",
        "fun f(i: MutableIterator<Int>) = i.forEachRemaining { println(it) }",
        "fun f(m: Map<String, Int>) = m.getOrDefault(\"k\", 1)",
        "fun f(m: Map<String, Int>) = m.forEach { (k, _) -> println(k) }",
        "fun f(s: Set<Int>) = s.stream()",
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            diagnostics.is_empty(),
            "kotlinc accepts this on the read-only face: {source}\n{diagnostics:?}"
        );
    }
}

#[test]
fn whitelisted_mutating_members_resolve_only_on_mutable_receivers() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for source in [
        "fun f(l: MutableList<Int>) { l.removeIf { true } }",
        "fun f(l: MutableList<Int>) { l.replaceAll { it + 1 } }",
        "fun f(l: MutableList<Int>) { l.addFirst(1) }",
        "fun f(l: MutableList<Int>) = l.removeLast()",
        "fun f(c: MutableCollection<Int>) { c.removeIf { true } }",
        "fun f(s: MutableSet<Int>) { s.removeIf { true } }",
        "fun f(m: MutableMap<String, Int>) = m.replace(\"k\", 1)",
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            diagnostics.is_empty(),
            "kotlinc accepts this on the mutable face: {source}\n{diagnostics:?}"
        );
    }
    // …and the same members stay hidden on the read-only face, exactly as kotlinc rejects them.
    for (member, source) in [
        ("addFirst", "fun f(l: List<Int>) { l.addFirst(1) }"),
        (
            "replaceAll",
            "fun f(m: Map<String, Int>) { m.replaceAll { k, v -> v } }",
        ),
        (
            "remove",
            "fun f(m: Map<String, Int>) { m.remove(\"k\", 1) }",
        ),
    ] {
        let diagnostics =
            common::front_end_diagnostics(source, std::slice::from_ref(&stdlib), Some(&jdk));
        assert!(
            diagnostics
                .iter()
                .any(|message| message.contains(&format!("unresolved reference '{member}'"))),
            "{member} must stay hidden on the read-only face (kotlinc rejects it): {diagnostics:?}"
        );
    }
}

#[test]
fn for_each_still_binds_the_inline_stdlib_extension() {
    // `MutableMap.forEach` is whitelisted, but a lambda with ONE (destructured `Map.Entry`)
    // parameter fits only the stdlib extension, and a plain lambda prefers the extension over the
    // SAM member — kotlinc inlines it. The destructure shape must keep compiling and running.
    let output = common::expect_box_run_with_stdlib(
        r#"
        fun box(): String {
            val m: MutableMap<String, Int> = mutableMapOf("a" to 1, "b" to 2)
            val out = StringBuilder()
            m.forEach { (k, v) -> out.append(k).append(v) }
            return out.toString()
        }
        "#,
        "Main",
    );
    assert_eq!(output, "a1b2");
}
