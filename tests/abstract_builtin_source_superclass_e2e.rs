//! Whether an abstract class materializes a builtin member its source superclass already
//! implements does not depend on the file that declares the superclass. `Child : Base(),
//! Collection<String>` introduces the `Collection` face itself and inherits `Base`'s concrete
//! `contains(String)` and `size`, so like kotlinc it writes only their final special bridges.
//! `GenericChild : GenericBase<String>(), Collection<String>` inherits `contains(element: T)`, which
//! stands for `contains(String)` in its applied signature, and leaves `size` abstract. The same
//! declarations in one file and split across two give kotlinc's complete classes and `box()`
//! result.

use super::common;

const BASE: &str = "package members

abstract class Base {
    fun contains(element: String): Boolean = element == \"a\"
    val size: Int get() = 1
}

abstract class GenericBase<T> {
    fun contains(element: T): Boolean = element == \"g\"
}
";

const CHILD: &str = "package members

abstract class Child : Base(), Collection<String>

abstract class GenericChild : GenericBase<String>(), Collection<String>

class Items : Child() {
    override fun isEmpty(): Boolean = false
    override fun containsAll(elements: Collection<String>): Boolean = elements.all { contains(it) }
    override fun iterator(): Iterator<String> = listOf(\"a\").iterator()
}

class GenericItems : GenericChild() {
    override val size: Int get() = 2
    override fun isEmpty(): Boolean = false
    override fun containsAll(elements: Collection<String>): Boolean = elements.all { contains(it) }
    override fun iterator(): Iterator<String> = listOf(\"g\", \"h\").iterator()
}

fun box(): String {
    val collection = java.util.Collection::class.java
    val contains = collection.getMethod(\"contains\", Any::class.java)
    val size = collection.getMethod(\"size\")
    val items = Items()
    val generic = GenericItems()
    if (contains.invoke(items, \"a\") != true) return \"contains a\"
    if (contains.invoke(items, \"b\") != false) return \"contains b\"
    if (contains.invoke(items, 1) != false) return \"contains 1\"
    if (size.invoke(items) != 1) return \"size\"
    if (contains.invoke(generic, \"g\") != true) return \"generic contains g\"
    if (contains.invoke(generic, 2) != false) return \"generic contains 2\"
    if (size.invoke(generic) != 2) return \"generic size\"
    if (!items.contains(\"a\") || generic.size != 2) return \"kotlin calls\"
    return \"OK\"
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
fn a_same_file_source_superclass_implements_the_builtin_members_like_kotlinc() {
    let source = format!("{BASE}{}", CHILD.trim_start_matches("package members\n"));
    assert_like_kotlinc(&[("Members.kt", &source)], "members.MembersKt");
}

#[test]
fn a_split_file_source_superclass_implements_the_builtin_members_like_kotlinc() {
    assert_like_kotlinc(&[("Base.kt", BASE), ("Child.kt", CHILD)], "members.ChildKt");
}
