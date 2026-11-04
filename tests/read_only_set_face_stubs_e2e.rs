//! A class that implements `kotlin.collections.Set` gets kotlinc's throwing stubs for the members
//! `MutableSet` declares: `add`, `remove`, `addAll`, `removeAll`, `retainAll` and `clear`, and
//! `iterator` when abstract. Each takes the type-safe and signature policies of the
//! `MutableCollection` declaration it overrides: `remove(Object)`, `addAll(Collection<? extends
//! E>)`, `removeAll`/`retainAll(Collection<?>)`. Unlike `Collection`, the face has no JDK
//! `removeIf` stub: kotlinc writes none for a `Set`.
//!
//! An abstract class's fake-override bridges are a later realization; these comparisons leave
//! them out by their exact spelling and compare every other member, the `toArray` pair and the
//! declared `containsAll` override included, byte for byte.

use super::common;

const MEMBERS: &str = "    override val size = 0
    override fun isEmpty() = true
    override fun contains(element: String) = false
    override fun containsAll(elements: Collection<String>) = false
    override fun iterator(): Iterator<String> = emptyList<String>().iterator()
";

fn string_set() -> String {
    format!("class Bag : Set<String> {{\n{MEMBERS}}}\n")
}

fn own_members_set() -> String {
    format!(
        "class Bag : Set<String> {{\n{MEMBERS}    fun add(element: String): Boolean = element.isEmpty()\n    fun clear() {{}}\n}}\n"
    )
}

const ABSTRACT_SET: &str = "abstract class Bag : Set<String>\n";

const ABSTRACT_INT_SET: &str = "abstract class Bag : Set<Int>\n";

const ABSTRACT_GENERIC_SET: &str = "abstract class Bag<T> : Set<T>\n";

/// The members a later realization writes, as `javap -p` spells them in either compiler's class.
const LATER_REALIZATIONS: &[&str] = &[
    "public abstract boolean contains(java.lang.String);",
    "public abstract boolean contains(int);",
    "public final boolean contains(java.lang.Object);",
    "public abstract int getSize();",
    "public final int size();",
];

fn assert_members_match_kotlinc(source: &str, members: &[&str]) {
    let classes =
        common::classes_against_kotlinc_module(&[("Bag.kt", &format!("package sets\n\n{source}"))]);
    let (reference, krusty) = classes
        .method_declarations("sets/Bag")
        .expect("both compilers write sets/Bag");
    let realized_here = |declarations: Vec<String>| {
        declarations
            .into_iter()
            .filter(|declaration| !LATER_REALIZATIONS.contains(&declaration.as_str()))
            .collect::<Vec<_>>()
    };
    assert_eq!(realized_here(krusty), realized_here(reference));
    let (reference, krusty) = classes.method_blocks("sets/Bag");
    for member in members {
        let block = |blocks: &[(String, String)]| {
            blocks
                .iter()
                .find(|(declaration, _)| declaration == member)
                .map(|(_, body)| body.clone())
                .unwrap_or_else(|| panic!("sets/Bag declares no `{member}`"))
        };
        assert_eq!(block(&krusty), block(&reference), "`{member}`");
    }
}

/// What each mutable `java.util.Set` member does when a Java caller reaches it.
fn box_program(source: &str, expected: &str) -> String {
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
    val bag: Any = Bag()
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

const READ_ONLY: &str =
    "UnsupportedOperationException: Operation is not supported for read-only collection";

#[test]
fn a_set_gets_kotlincs_mutable_set_stubs() {
    assert_members_match_kotlinc(
        &string_set(),
        &[
            "public boolean add(java.lang.String);",
            "public boolean remove(java.lang.Object);",
            "public boolean addAll(java.util.Collection<? extends java.lang.String>);",
            "public boolean removeAll(java.util.Collection<?>);",
            "public boolean retainAll(java.util.Collection<?>);",
            "public void clear();",
            "public boolean add(java.lang.Object);",
        ],
    );
    let expected = ["add", "remove", "addAll", "removeAll", "retainAll", "clear"]
        .map(|name| format!("{name}: {READ_ONLY}"))
        .join("; ");
    common::expect_box_same_as_kotlinc(
        &box_program(&string_set(), &expected),
        "read_only_set_face_string",
    );
}

#[test]
fn own_set_members_implement_their_stubs_like_kotlinc() {
    assert_members_match_kotlinc(
        &own_members_set(),
        &[
            "public final boolean add(java.lang.String);",
            "public final void clear();",
            "public boolean remove(java.lang.Object);",
            "public boolean add(java.lang.Object);",
        ],
    );
    let expected = format!(
        "add=true; remove: {READ_ONLY}; addAll: {READ_ONLY}; removeAll: {READ_ONLY}; retainAll: {READ_ONLY}; clear=null"
    );
    common::expect_box_same_as_kotlinc(
        &box_program(&own_members_set(), &expected),
        "read_only_set_face_own_members",
    );
}

#[test]
fn an_abstract_set_also_gets_an_iterator_stub_like_kotlinc() {
    assert_members_match_kotlinc(
        ABSTRACT_SET,
        &[
            "public java.util.Iterator<java.lang.String> iterator();",
            "public boolean add(java.lang.String);",
            "public boolean add(java.lang.Object);",
        ],
    );
}

#[test]
fn an_abstract_primitive_set_keeps_its_unboxed_add_and_bridge_like_kotlinc() {
    assert_members_match_kotlinc(
        ABSTRACT_INT_SET,
        &[
            "public java.util.Iterator<java.lang.Integer> iterator();",
            "public boolean add(int);",
            "public boolean addAll(java.util.Collection<? extends java.lang.Integer>);",
            "public boolean add(java.lang.Object);",
        ],
    );
}

#[test]
fn an_abstract_generic_set_needs_no_add_bridge_like_kotlinc() {
    assert_members_match_kotlinc(
        ABSTRACT_GENERIC_SET,
        &[
            "public java.util.Iterator<T> iterator();",
            "public boolean add(T);",
            "public boolean addAll(java.util.Collection<? extends T>);",
            "public boolean removeAll(java.util.Collection<?>);",
        ],
    );
}
