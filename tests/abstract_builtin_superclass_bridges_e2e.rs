//! A subclass inherits the final special bridges its abstract collection superclass writes for the
//! builtin members it leaves abstract, whichever file declares that superclass. `Sized<E>` writes
//! `abstract getSize()` and the final `size()` that calls it; `Strings` also writes the abstract
//! `contains(String)` and the type-checking final `contains(Object)`. A subclass that implements
//! the members must not write those final bridges again, or the JVM rejects it
//! (`IncompatibleClassChangeError: overrides final method`). Bridges are a JVM realization, so the
//! fixture is JVM-specific: the same declarations in one file and split across two give kotlinc's
//! complete classes and `box()` result, and Java reaches the members through `java.util.Collection`.

use super::common;

const BASES: &str = "package bridges

abstract class Sized<E> : MutableCollection<E>

abstract class Strings : MutableCollection<String>
";

const SUBCLASSES: &str = "package bridges

class ListQueue<E> : Sized<E>() {
    private val items = ArrayList<E>()
    override val size: Int get() = items.size
    override fun isEmpty(): Boolean = items.isEmpty()
    override fun contains(element: E): Boolean = items.contains(element)
    override fun containsAll(elements: Collection<E>): Boolean = items.containsAll(elements)
    override fun iterator(): MutableIterator<E> = items.iterator()
    override fun add(element: E): Boolean = items.add(element)
    override fun addAll(elements: Collection<E>): Boolean = items.addAll(elements)
    override fun remove(element: E): Boolean = items.remove(element)
    override fun removeAll(elements: Collection<E>): Boolean = items.removeAll(elements)
    override fun retainAll(elements: Collection<E>): Boolean = items.retainAll(elements)
    override fun clear() = items.clear()
}

class StringQueue : Strings() {
    private val items = ArrayList<String>()
    override val size: Int get() = items.size
    override fun isEmpty(): Boolean = items.isEmpty()
    override fun contains(element: String): Boolean = items.contains(element)
    override fun containsAll(elements: Collection<String>): Boolean = items.containsAll(elements)
    override fun iterator(): MutableIterator<String> = items.iterator()
    override fun add(element: String): Boolean = items.add(element)
    override fun addAll(elements: Collection<String>): Boolean = items.addAll(elements)
    override fun remove(element: String): Boolean = items.remove(element)
    override fun removeAll(elements: Collection<String>): Boolean = items.removeAll(elements)
    override fun retainAll(elements: Collection<String>): Boolean = items.retainAll(elements)
    override fun clear() = items.clear()
}

fun box(): String {
    val collection = java.util.Collection::class.java
    val contains = collection.getMethod(\"contains\", Any::class.java)
    val size = collection.getMethod(\"size\")
    val queue = ListQueue<String>()
    queue.add(\"O\")
    val strings = StringQueue()
    strings.add(\"K\")
    if (size.invoke(queue) != 1 || size.invoke(strings) != 1) return \"java size\"
    if (contains.invoke(queue, \"O\") != true) return \"java contains O\"
    if (contains.invoke(strings, \"K\") != true) return \"java contains K\"
    if (contains.invoke(strings, 1) != false) return \"java contains 1\"
    if (queue.size != 1 || \"K\" !in strings) return \"kotlin calls\"
    return queue.first() + strings.first()
}
";

fn assert_like_kotlinc(sources: &[(&str, &str)], main_class: &str) {
    let classes = common::classes_against_kotlinc_module(sources);
    assert_eq!(classes.differences(), Vec::<String>::new());
    let reference = common::kotlinc_box_files_result(sources, main_class);
    assert_eq!(reference, "OK", "kotlinc runs the fixture");
    let krusty =
        common::compile_and_run_files_with_stdlib(sources).expect("krusty runs the fixture");
    assert_eq!(krusty, reference);
}

#[test]
fn a_same_file_abstract_collection_superclass_keeps_its_final_bridges_like_kotlinc() {
    let source = format!(
        "{BASES}{}",
        SUBCLASSES.trim_start_matches("package bridges\n")
    );
    assert_like_kotlinc(&[("Bridges.kt", &source)], "bridges.BridgesKt");
}

#[test]
fn a_split_file_abstract_collection_superclass_keeps_its_final_bridges_like_kotlinc() {
    assert_like_kotlinc(
        &[("Bases.kt", BASES), ("Subclasses.kt", SUBCLASSES)],
        "bridges.SubclassesKt",
    );
}
