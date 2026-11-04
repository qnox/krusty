//! A class that implements `kotlin.collections.List` gets kotlinc's throwing stubs for the
//! members `MutableList` declares: `add`, `add(Int, E)`, `remove`, both `addAll`, `removeAll`,
//! `retainAll`, `clear`, `set` and `removeAt`, and, when abstract, `listIterator` and `subList`.
//! One name's overloads stay together, in their first declaration's place. The type-safe and
//! signature policies of the `MutableCollection` declarations a member overrides apply to it as
//! well. A scalar result over the generic one is boxed (`Integer set(int, int)`). `removeAt` is
//! realized under its JVM name, `remove(int)`, unboxed, with its throwing `Object remove(int)`
//! bridge right after it. A class's own `removeAt` instead gets kotlinc's final special
//! `remove(int)` bridge and the erased one. The override check sees applied supertypes, so a
//! `MutableListIterator<String>` result implements `listIterator`.
//!
//! The JDK defaults (`replaceAll`, `sort`, the `SequencedCollection` members) are later
//! realizations, and so are a declared `containsAll` override's signature and an abstract class's
//! fake-override bridges; these comparisons leave them out by their exact spelling and compare
//! every other member, the `toArray` pair included, byte for byte.

use super::common;

fn members(element: &str, zero: &str) -> String {
    format!(
        "    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: {element}) = false
    override fun containsAll(elements: Collection<{element}>) = false
    override fun iterator(): Iterator<{element}> = emptyList<{element}>().iterator()
    override fun get(index: Int): {element} = {zero}
    override fun indexOf(element: {element}) = -1
    override fun lastIndexOf(element: {element}) = -1
    override fun listIterator(): ListIterator<{element}> = emptyList<{element}>().listIterator()
    override fun listIterator(index: Int): ListIterator<{element}> =
        emptyList<{element}>().listIterator(index)
    override fun subList(fromIndex: Int, toIndex: Int): List<{element}> = emptyList()
"
    )
}

fn string_row() -> String {
    format!(
        "class Row : List<String> {{\n{}}}\n",
        members("String", "\"\"")
    )
}

fn int_row() -> String {
    format!("class Row : List<Int> {{\n{}}}\n", members("Int", "0"))
}

fn own_members_row() -> String {
    format!(
        "class Row : List<String> {{\n{}    fun set(index: Int, element: String): String = element
    fun removeAt(index: Int): String = \"removed $index\"
    fun add(index: Int, element: String) {{}}\n}}\n",
        members("String", "\"\"")
    )
}

/// A `List<Any>`'s `removeAt` erases like the throwing stub would, but its `String` result is
/// narrower, so `Object remove(int)` still bridges to the special `String remove(int)`.
fn narrower_remove_at_row() -> String {
    format!(
        "class Row : List<Any> {{\n{}    fun removeAt(index: Int): String = \"removed $index\"\n}}\n",
        members("Any", "\"\"")
    )
}

fn other_result_row() -> String {
    format!(
        "class Row : List<String> {{\n{}    fun set(index: Int, element: String): Int = 1\n}}\n",
        members("String", "\"\"")
    )
}

fn mutable_iterator_row() -> String {
    let members = members("String", "\"\"").replace(
        "override fun listIterator(): ListIterator<String> = emptyList<String>().listIterator()",
        "override fun listIterator(): MutableListIterator<String> =\n        mutableListOf<String>().listIterator()",
    );
    format!("class Row : List<String> {{\n{members}}}\n")
}

const ABSTRACT_ROW: &str = "abstract class Row : List<String>\n";

const ABSTRACT_GENERIC_ROW: &str = "abstract class Row<T> : List<T>\n";

const SOURCE_ITERATOR_ROW: &str = "abstract class Walk : ListIterator<String>

abstract class Row : List<String> {
    abstract override fun listIterator(): Walk
}
";

const ABSTRACT_INT_ITERATOR: &str = "abstract class Row : ListIterator<Int>\n";

/// The members a later realization writes, as `javap -p` spells them in either compiler's class.
const LATER_REALIZATIONS: &[&str] = &[
    "public boolean containsAll(java.util.Collection<?>);",
    "public boolean containsAll(java.util.Collection<java.lang.String>);",
    "public boolean containsAll(java.util.Collection<java.lang.Integer>);",
    "public void replaceAll(java.util.function.UnaryOperator<java.lang.String>);",
    "public void replaceAll(java.util.function.UnaryOperator<java.lang.Integer>);",
    "public void replaceAll(java.util.function.UnaryOperator<T>);",
    "public void sort(java.util.Comparator<? super java.lang.String>);",
    "public void sort(java.util.Comparator<? super java.lang.Integer>);",
    "public void sort(java.util.Comparator<? super T>);",
    "public void addFirst(java.lang.String);",
    "public void addFirst(java.lang.Integer);",
    "public void addFirst(T);",
    "public void addFirst(java.lang.Object);",
    "public void addLast(java.lang.String);",
    "public void addLast(java.lang.Integer);",
    "public void addLast(T);",
    "public void addLast(java.lang.Object);",
    "public java.lang.String removeFirst();",
    "public java.lang.Integer removeFirst();",
    "public T removeFirst();",
    "public java.lang.Object removeFirst();",
    "public java.lang.String removeLast();",
    "public java.lang.Integer removeLast();",
    "public T removeLast();",
    "public java.lang.Object removeLast();",
];

/// The fake-override members an abstract class's unimplemented read-only members get.
const ABSTRACT_FAKE_OVERRIDE_BRIDGES: &[&str] = &[
    "public abstract boolean contains(java.lang.String);",
    "public final boolean contains(java.lang.Object);",
    "public abstract int indexOf(java.lang.String);",
    "public final int indexOf(java.lang.Object);",
    "public abstract int lastIndexOf(java.lang.String);",
    "public final int lastIndexOf(java.lang.Object);",
    "public abstract int getSize();",
    "public final int size();",
];

fn assert_members_match_kotlinc(source: &str, members: &[&str], later: &[&str]) {
    let classes = common::classes_against_kotlinc_module(&[(
        "Row.kt",
        &format!("package lists\n\n{source}"),
    )]);
    let (reference, krusty) = classes
        .method_declarations("lists/Row")
        .expect("both compilers write lists/Row");
    let realized_here = |declarations: Vec<String>| {
        declarations
            .into_iter()
            .filter(|declaration| {
                !LATER_REALIZATIONS.contains(&declaration.as_str())
                    && !later.contains(&declaration.as_str())
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(realized_here(krusty), realized_here(reference));
    let (reference, krusty) = classes.method_blocks("lists/Row");
    for member in members {
        let block = |blocks: &[(String, String)]| {
            blocks
                .iter()
                .find(|(declaration, _)| declaration == member)
                .map(|(_, body)| body.clone())
                .unwrap_or_else(|| panic!("lists/Row declares no `{member}`"))
        };
        assert_eq!(block(&krusty), block(&reference), "`{member}`");
    }
}

/// What each mutable `java.util.List` member does when a Java caller reaches it.
fn box_program(source: &str, element: &str, expected: &str) -> String {
    format!(
        "{source}
fun outcome(row: Any, name: String, parameters: List<Class<*>>, arguments: List<Any?>): String = try {{
    val result = row.javaClass.getMethod(name, *parameters.toTypedArray())
        .invoke(row, *arguments.toTypedArray())
    \"$name=$result\"
}} catch (e: java.lang.reflect.InvocationTargetException) {{
    \"$name: ${{e.cause!!::class.simpleName}}\"
}}

fun box(): String {{
    val row: Any = Row()
    val index = Int::class.javaPrimitiveType!!
    val any = Any::class.java
    val element: Any = {element}
    val outcomes = listOf(
        outcome(row, \"add\", listOf(any), listOf(element)),
        outcome(row, \"add\", listOf(index, any), listOf(0, element)),
        outcome(row, \"remove\", listOf(any), listOf(element)),
        outcome(row, \"addAll\", listOf(index, Collection::class.java), listOf(0, listOf(element))),
        outcome(row, \"set\", listOf(index, any), listOf(0, element)),
        outcome(row, \"remove\", listOf(index), listOf(1)),
        outcome(row, \"clear\", listOf(), listOf()),
    ).joinToString(\"; \")
    return if (outcomes == \"{expected}\") \"OK\" else outcomes
}}
"
    )
}

fn all_read_only() -> String {
    ["add", "add", "remove", "addAll", "set", "remove", "clear"]
        .map(|name| format!("{name}: UnsupportedOperationException"))
        .join("; ")
}

#[test]
fn a_list_gets_kotlincs_mutable_list_stubs() {
    assert_members_match_kotlinc(
        &string_row(),
        &[
            "public boolean add(java.lang.String);",
            "public void add(int, java.lang.String);",
            "public boolean remove(java.lang.Object);",
            "public boolean addAll(java.util.Collection<? extends java.lang.String>);",
            "public boolean addAll(int, java.util.Collection<? extends java.lang.String>);",
            "public boolean removeAll(java.util.Collection<?>);",
            "public boolean retainAll(java.util.Collection<?>);",
            "public void clear();",
            "public java.lang.String set(int, java.lang.String);",
            "public java.lang.String remove(int);",
            "public java.lang.Object remove(int);",
            "public boolean add(java.lang.Object);",
            "public void add(int, java.lang.Object);",
            "public java.lang.Object set(int, java.lang.Object);",
        ],
        &[],
    );
    common::expect_box_same_as_kotlinc(
        &box_program(&string_row(), "\"x\"", &all_read_only()),
        "read_only_list_face_string",
    );
}

#[test]
fn a_primitive_list_boxes_set_but_not_remove_at_like_kotlinc() {
    assert_members_match_kotlinc(
        &int_row(),
        &[
            "public boolean add(int);",
            "public void add(int, int);",
            "public java.lang.Integer set(int, int);",
            "public int remove(int);",
            "public java.lang.Object remove(int);",
            "public boolean add(java.lang.Object);",
            "public void add(int, java.lang.Object);",
            "public java.lang.Object set(int, java.lang.Object);",
        ],
        &[],
    );
    common::expect_box_same_as_kotlinc(
        &box_program(&int_row(), "7", &all_read_only()),
        "read_only_list_face_int",
    );
}

#[test]
fn own_list_members_implement_their_stubs_and_remove_at_gets_its_special_bridge_like_kotlinc() {
    assert_members_match_kotlinc(
        &own_members_row(),
        &[
            "public final java.lang.String set(int, java.lang.String);",
            "public final java.lang.String removeAt(int);",
            "public final void add(int, java.lang.String);",
            "public java.lang.Object set(int, java.lang.Object);",
            "public final java.lang.String remove(int);",
            "public java.lang.Object remove(int);",
            "public void add(int, java.lang.Object);",
            "public boolean add(java.lang.Object);",
        ],
        &[],
    );
    let expected = "add: UnsupportedOperationException; add=null; \
        remove: UnsupportedOperationException; addAll: UnsupportedOperationException; \
        set=x; remove=removed 1; clear: UnsupportedOperationException";
    common::expect_box_same_as_kotlinc(
        &box_program(&own_members_row(), "\"x\"", expected),
        "read_only_list_face_own_members",
    );
}

#[test]
fn a_set_with_another_result_keeps_the_list_stub_like_kotlinc() {
    assert_members_match_kotlinc(
        &other_result_row(),
        &[
            "public final int set(int, java.lang.String);",
            "public java.lang.String set(int, java.lang.String);",
            "public java.lang.Object set(int, java.lang.Object);",
        ],
        &[],
    );
}

#[test]
fn a_mutable_list_iterator_result_implements_list_iterator_like_kotlinc() {
    assert_members_match_kotlinc(
        &mutable_iterator_row(),
        &["public java.util.ListIterator<java.lang.String> listIterator();"],
        &[],
    );
}

#[test]
fn an_abstract_list_also_gets_list_iterator_and_sub_list_stubs_like_kotlinc() {
    assert_members_match_kotlinc(
        ABSTRACT_ROW,
        &[
            "public java.lang.String remove(int);",
            "public java.lang.Object remove(int);",
            "public java.util.ListIterator<java.lang.String> listIterator();",
            "public java.util.ListIterator<java.lang.String> listIterator(int);",
            "public java.util.List<java.lang.String> subList(int, int);",
        ],
        ABSTRACT_FAKE_OVERRIDE_BRIDGES,
    );
}

#[test]
fn an_abstract_generic_list_needs_no_erased_bridges_like_kotlinc() {
    assert_members_match_kotlinc(
        ABSTRACT_GENERIC_ROW,
        &[
            "public boolean add(T);",
            "public void add(int, T);",
            "public T set(int, T);",
            "public T remove(int);",
            "public java.util.ListIterator<T> listIterator();",
            "public java.util.List<T> subList(int, int);",
        ],
        &["public abstract int getSize();", "public final int size();"],
    );
}

#[test]
fn a_source_list_iterator_result_implements_list_iterator_like_kotlinc() {
    assert_members_match_kotlinc(
        SOURCE_ITERATOR_ROW,
        &[
            "public java.util.ListIterator<java.lang.String> listIterator(int);",
            "public java.util.List<java.lang.String> subList(int, int);",
        ],
        ABSTRACT_FAKE_OVERRIDE_BRIDGES,
    );
}

#[test]
fn an_abstract_primitive_list_iterator_boxes_next_like_kotlinc() {
    assert_members_match_kotlinc(
        ABSTRACT_INT_ITERATOR,
        &[
            "public java.lang.Integer next();",
            "public void set(int);",
            "public void add(int);",
            "public java.lang.Object next();",
        ],
        &[],
    );
}

#[test]
fn a_narrower_remove_at_keeps_its_erased_remove_bridge_like_kotlinc() {
    assert_members_match_kotlinc(
        &narrower_remove_at_row(),
        &[
            "public final java.lang.String removeAt(int);",
            "public final java.lang.String remove(int);",
            "public java.lang.Object remove(int);",
        ],
        // The `Any` spellings of the later realizations.
        &[
            "public boolean containsAll(java.util.Collection<? extends java.lang.Object>);",
            "public void replaceAll(java.util.function.UnaryOperator<java.lang.Object>);",
            "public void sort(java.util.Comparator<? super java.lang.Object>);",
        ],
    );
    let expected = "add: UnsupportedOperationException; add: UnsupportedOperationException; \
        remove: UnsupportedOperationException; addAll: UnsupportedOperationException; \
        set: UnsupportedOperationException; remove=removed 1; \
        clear: UnsupportedOperationException";
    common::expect_box_same_as_kotlinc(
        &box_program(&narrower_remove_at_row(), "\"x\"", expected),
        "read_only_list_face_narrower_remove_at",
    );
}
