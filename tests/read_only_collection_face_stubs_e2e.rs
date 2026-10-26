//! A class that implements `kotlin.collections.Collection` gets kotlinc's throwing stubs for the
//! mutable members of `java.util.Collection`: `add`, `remove`, `addAll`, `removeAll`, `retainAll`
//! and `clear`. Each is written in kotlinc's signature for its declaration: `remove(Object)`
//! type-safely erased, `addAll(Collection<? extends E>)` with its declaration-site wildcard, and
//! `removeAll`/`retainAll(Collection<?>)` star-projected. A member the class declares itself
//! implements its stub, and the erased `add(Object)` bridge forwards to it, among the bridges in
//! member order. An abstract class gets a throwing `iterator()` stub as well.
//!
//! The JDK default `removeIf` and the `toArray` pair are later realizations, and so is the
//! signature of a declared `containsAll` override; these comparisons leave those members out by
//! their exact spelling and compare every other member byte for byte.

use super::common;

const MEMBERS: &str = "    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: String) = false
    override fun containsAll(elements: Collection<String>) = false
    override fun iterator(): Iterator<String> = emptyList<String>().iterator()
";

fn string_bag() -> String {
    format!("class Bag : Collection<String> {{\n{MEMBERS}}}\n")
}

fn int_bag() -> String {
    "class Bag : Collection<Int> {
    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: Int) = false
    override fun containsAll(elements: Collection<Int>) = false
    override fun iterator(): Iterator<Int> = emptyList<Int>().iterator()
}
"
    .to_string()
}

fn generic_bag() -> String {
    "class Bag<T> : Collection<T> {
    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: T) = false
    override fun containsAll(elements: Collection<T>) = false
    override fun iterator(): Iterator<T> = emptyList<T>().iterator()
}
"
    .to_string()
}

fn own_members_bag() -> String {
    format!(
        "class Bag : Collection<String> {{\n{MEMBERS}    fun add(element: String): Boolean = element.isEmpty()\n    fun clear() {{}}\n}}\n"
    )
}

const ABSTRACT_BAG: &str = "abstract class Bag : Collection<String>\n";

/// The members a later realization writes, as `javap -p` spells them in either compiler's class.
const LATER_REALIZATIONS: &[&str] = &[
    "public boolean containsAll(java.util.Collection<?>);",
    "public boolean containsAll(java.util.Collection<java.lang.String>);",
    "public boolean containsAll(java.util.Collection<java.lang.Integer>);",
    "public boolean containsAll(java.util.Collection<? extends T>);",
    "public boolean removeIf(java.util.function.Predicate<? super java.lang.String>);",
    "public boolean removeIf(java.util.function.Predicate<? super java.lang.Integer>);",
    "public boolean removeIf(java.util.function.Predicate<? super T>);",
    "public <T> T[] toArray(T[]);",
    "public java.lang.Object[] toArray();",
];

/// The fake-override bridges an abstract class's unimplemented `contains` and `size` get, a later
/// realization as well.
const ABSTRACT_FAKE_OVERRIDE_BRIDGES: &[&str] = &[
    "public abstract boolean contains(java.lang.String);",
    "public final boolean contains(java.lang.Object);",
    "public abstract int getSize();",
    "public final int size();",
];

/// The signature of a generic class's declared `contains(element: T)`, a later realization too:
/// kotlinc writes the type-safe member without one.
const GENERIC_DECLARED_CONTAINS: &[&str] = &[
    "public boolean contains(T);",
    "public boolean contains(java.lang.Object);",
];

fn assert_members_match_kotlinc(source: &str, members: &[&str]) {
    assert_members_match_kotlinc_beside(source, members, &[]);
}

fn assert_members_match_kotlinc_beside(source: &str, members: &[&str], later: &[&str]) {
    let classes = common::classes_against_kotlinc_module(&[(
        "Bag.kt",
        &format!("package faces\n\n{source}"),
    )]);
    let (reference, krusty) = classes
        .method_declarations("faces/Bag")
        .expect("both compilers write faces/Bag");
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
    let (reference, krusty) = classes.method_blocks("faces/Bag");
    for member in members {
        let block = |blocks: &[(String, String)]| {
            blocks
                .iter()
                .find(|(declaration, _)| declaration == member)
                .map(|(_, body)| body.clone())
                .unwrap_or_else(|| panic!("faces/Bag declares no `{member}`"))
        };
        assert_eq!(block(&krusty), block(&reference), "`{member}`");
    }
}

const STUBS: &[&str] = &[
    "public boolean add(java.lang.String);",
    "public boolean remove(java.lang.Object);",
    "public boolean addAll(java.util.Collection<? extends java.lang.String>);",
    "public boolean removeAll(java.util.Collection<?>);",
    "public boolean retainAll(java.util.Collection<?>);",
    "public void clear();",
    "public final int size();",
    "public final boolean contains(java.lang.Object);",
    "public boolean add(java.lang.Object);",
];

/// What each mutable `java.util.Collection` member does when a Java caller reaches it.
fn box_program(source: &str, construct: &str, expected: &str) -> String {
    format!(
        "{source}
fun outcome(bag: Any, name: String, parameter: Class<*>?, argument: Any?): String = try {{
    val method = if (parameter == null) bag.javaClass.getMethod(name)
        else bag.javaClass.getMethod(name, parameter)
    val result = if (parameter == null) method.invoke(bag) else method.invoke(bag, argument)
    \"$name=$result\"
}} catch (e: java.lang.reflect.InvocationTargetException) {{
    \"$name: ${{e.cause!!::class.simpleName}}: ${{e.cause!!.message}}\"
}}

fun box(): String {{
    val bag: Any = {construct}
    val outcomes = listOf(
        outcome(bag, \"add\", Any::class.java, \"\"),
        outcome(bag, \"remove\", Any::class.java, \"\"),
        outcome(bag, \"addAll\", Collection::class.java, listOf(\"\")),
        outcome(bag, \"removeAll\", Collection::class.java, listOf(\"\")),
        outcome(bag, \"retainAll\", Collection::class.java, listOf(\"\")),
        outcome(bag, \"clear\", null, null),
    ).joinToString(\"; \")
    return if (outcomes == \"{expected}\") \"OK\" else outcomes
}}
"
    )
}

const READ_ONLY: &str = "Operation is not supported for read-only collection";

fn all_read_only() -> String {
    ["add", "remove", "addAll", "removeAll", "retainAll", "clear"]
        .map(|name| format!("{name}: UnsupportedOperationException: {READ_ONLY}"))
        .join("; ")
}

#[test]
fn a_collection_gets_kotlincs_mutable_member_stubs() {
    assert_members_match_kotlinc(&string_bag(), STUBS);
    common::expect_box_same_as_kotlinc(
        &box_program(&string_bag(), "Bag()", &all_read_only()),
        "read_only_collection_face_string",
    );
}

#[test]
fn a_primitive_collection_stub_keeps_its_unboxed_add_and_bridge_like_kotlinc() {
    assert_members_match_kotlinc(
        &int_bag(),
        &[
            "public boolean add(int);",
            "public boolean remove(java.lang.Object);",
            "public boolean addAll(java.util.Collection<? extends java.lang.Integer>);",
            "public boolean removeAll(java.util.Collection<?>);",
            "public boolean retainAll(java.util.Collection<?>);",
            "public void clear();",
            "public boolean add(java.lang.Object);",
        ],
    );
}

#[test]
fn a_generic_collection_stub_needs_no_add_bridge_like_kotlinc() {
    assert_members_match_kotlinc_beside(
        &generic_bag(),
        &[
            "public boolean add(T);",
            "public boolean remove(java.lang.Object);",
            "public boolean addAll(java.util.Collection<? extends T>);",
            "public boolean removeAll(java.util.Collection<?>);",
            "public boolean retainAll(java.util.Collection<?>);",
            "public void clear();",
        ],
        GENERIC_DECLARED_CONTAINS,
    );
    common::expect_box_same_as_kotlinc(
        &box_program(&generic_bag(), "Bag<String>()", &all_read_only()),
        "read_only_collection_face_generic",
    );
}

#[test]
fn own_collection_members_implement_their_stubs_and_the_add_bridge_forwards_like_kotlinc() {
    assert_members_match_kotlinc(
        &own_members_bag(),
        &[
            "public final boolean add(java.lang.String);",
            "public final void clear();",
            "public boolean remove(java.lang.Object);",
            "public boolean addAll(java.util.Collection<? extends java.lang.String>);",
            "public final int size();",
            "public final boolean contains(java.lang.Object);",
            "public boolean add(java.lang.Object);",
        ],
    );
    let expected = format!(
        "add=true; remove: UnsupportedOperationException: {READ_ONLY}; addAll: UnsupportedOperationException: {READ_ONLY}; removeAll: UnsupportedOperationException: {READ_ONLY}; retainAll: UnsupportedOperationException: {READ_ONLY}; clear=null"
    );
    common::expect_box_same_as_kotlinc(
        &box_program(&own_members_bag(), "Bag()", &expected),
        "read_only_collection_face_own_members",
    );
}

#[test]
fn an_abstract_collection_also_gets_an_iterator_stub_like_kotlinc() {
    assert_members_match_kotlinc_beside(
        ABSTRACT_BAG,
        &[
            "public java.util.Iterator<java.lang.String> iterator();",
            "public boolean add(java.lang.String);",
            "public boolean remove(java.lang.Object);",
            "public boolean addAll(java.util.Collection<? extends java.lang.String>);",
            "public boolean removeAll(java.util.Collection<?>);",
            "public boolean retainAll(java.util.Collection<?>);",
            "public void clear();",
        ],
        ABSTRACT_FAKE_OVERRIDE_BRIDGES,
    );
}
