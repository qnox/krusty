//! A mapped Kotlin collection's member scope is its `.kotlin_builtins` declaration, not the method set of
//! the `java.util` class it maps to. `java.util.List` declares `remove(int)` (remove BY INDEX) alongside
//! `remove(Object)` (remove the ELEMENT); Kotlin's `MutableList` declares only
//! `MutableCollection.remove(element: E)`, with the index-taking method reachable solely as `removeAt`.
//! Taking the Java set MISCOMPILED `list.remove(10)` — an `Int` argument fits the primitive `int`
//! parameter exactly, so it won overload resolution and removed whichever element sat at index 10.
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
    let Some(output) = common::compile_and_run_with_stdlib(
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
    ) else {
        panic!("compile/run returned None");
    };
    assert_eq!(output, "100 200 [3]");
}

#[test]
fn java_only_members_are_not_in_the_kotlin_scope() {
    // The members the JVM class declares and the Kotlin API does not. Each of these compiled before the
    // scope came from the builtins declaration; kotlinc reports every one as unresolved.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for (member, source) in [
        ("stream", "fun f(l: List<Int>) { l.stream() }"),
        ("getFirst", "fun f(l: List<Int>) { l.getFirst() }"),
        ("spliterator", "fun f(l: List<Int>) { l.spliterator() }"),
        ("add", "fun f(l: List<Int>) { l.add(1) }"),
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
