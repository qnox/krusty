//! A class that implements `kotlin.collections.Map` gets kotlinc's throwing stubs for the members
//! `MutableMap` declares: `put`, `remove(key)`, `remove(key, value)`, `putAll` and `clear`, each
//! with the policies of its declaration (`remove(Object)`, `putAll(Map<? extends K, ? extends
//! V>)`) and a boxed scalar result over the generic one (`Integer put(String, int)`). A
//! `Map.Entry` gets `setValue` the same way (`Integer setValue(int)`).
//!
//! The map's special bridges take kotlinc's form: a renamed getter's bridge carries its result's
//! generic signature (`Set<String> keySet()`), and a special bridge whose implementation result
//! is narrower than the erased one is two methods: `final Integer get(Object)` after a final
//! synthetic `Object get(Object)` copy of it, and `final HashSet<String> keySet()` before an
//! ordinary `Set keySet()` bridge to it.
//!
//! The JDK defaults (`replaceAll`, `putIfAbsent`, `replace`, `compute*`, `merge`) and an abstract
//! class's fake-override bridges are later realizations; these comparisons leave them out by
//! their exact spelling and compare every other member byte for byte.

use super::common;

const MEMBERS: &str = "    override val size = 0
    override fun isEmpty() = true
    override fun containsKey(key: String) = false
    override fun containsValue(value: Int) = false
    override fun get(key: String): Int? = null
    override val keys: Set<String> = emptySet()
    override val values: Collection<Int> = emptyList()
    override val entries: Set<Map.Entry<String, Int>> = emptySet()
";

fn string_map() -> String {
    format!("class Dict : Map<String, Int> {{\n{MEMBERS}}}\n")
}

fn own_members_map() -> String {
    format!(
        "class Dict : Map<String, Int> {{\n{MEMBERS}    fun put(key: String, value: Int): Int? = value\n    fun clear() {{}}\n}}\n"
    )
}

fn hash_set_keys_map() -> String {
    format!(
        "class Dict : Map<String, Int> {{\n{}}}\n",
        MEMBERS.replace(
            "override val keys: Set<String> = emptySet()",
            "override val keys: HashSet<String> = hashSetOf()",
        )
    )
}

const ENTRY: &str = "class Dict : Map.Entry<String, Int> {
    override val key = \"\"
    override val value = 1
}
";

const GENERIC_ENTRY: &str = "class Dict<V>(override val value: V) : Map.Entry<String, V> {
    override val key = \"\"
}
";

const ABSTRACT_ENTRY: &str = "abstract class Dict : Map.Entry<String, Int>\n";

/// The members a later realization writes, as `javap -p` spells them in either compiler's class.
const LATER_REALIZATIONS: &[&str] = &[
    "public void replaceAll(java.util.function.BiFunction<? super java.lang.String, ? super java.lang.Integer, ? extends java.lang.Integer>);",
    "public java.lang.Integer putIfAbsent(java.lang.String, java.lang.Integer);",
    "public boolean replace(java.lang.String, java.lang.Integer, java.lang.Integer);",
    "public java.lang.Integer replace(java.lang.String, java.lang.Integer);",
    "public java.lang.Integer computeIfAbsent(java.lang.String, java.util.function.Function<? super java.lang.String, ? extends java.lang.Integer>);",
    "public java.lang.Integer computeIfPresent(java.lang.String, java.util.function.BiFunction<? super java.lang.String, ? super java.lang.Integer, ? extends java.lang.Integer>);",
    "public java.lang.Integer compute(java.lang.String, java.util.function.BiFunction<? super java.lang.String, ? super java.lang.Integer, ? extends java.lang.Integer>);",
    "public java.lang.Integer merge(java.lang.String, java.lang.Integer, java.util.function.BiFunction<? super java.lang.Integer, ? super java.lang.Integer, ? extends java.lang.Integer>);",
    "public java.lang.Object putIfAbsent(java.lang.Object, java.lang.Object);",
    "public boolean replace(java.lang.Object, java.lang.Object, java.lang.Object);",
    "public java.lang.Object replace(java.lang.Object, java.lang.Object);",
    "public java.lang.Object computeIfAbsent(java.lang.Object, java.util.function.Function);",
    "public java.lang.Object computeIfPresent(java.lang.Object, java.util.function.BiFunction);",
    "public java.lang.Object compute(java.lang.Object, java.util.function.BiFunction);",
    "public java.lang.Object merge(java.lang.Object, java.lang.Object, java.util.function.BiFunction);",
];

fn assert_members_match_kotlinc(source: &str, members: &[&str]) {
    let classes = common::classes_against_kotlinc_module(&[(
        "Dict.kt",
        &format!("package maps\n\n{source}"),
    )]);
    let (reference, krusty) = classes
        .method_declarations("maps/Dict")
        .expect("both compilers write maps/Dict");
    let realized_here = |declarations: Vec<String>| {
        declarations
            .into_iter()
            .filter(|declaration| !LATER_REALIZATIONS.contains(&declaration.as_str()))
            .collect::<Vec<_>>()
    };
    assert_eq!(realized_here(krusty), realized_here(reference));
    let (reference, krusty) = classes.method_blocks("maps/Dict");
    for member in members {
        let block = |blocks: &[(String, String)]| {
            blocks
                .iter()
                .find(|(declaration, _)| declaration == member)
                .map(|(_, body)| body.clone())
                .unwrap_or_else(|| panic!("maps/Dict declares no `{member}`"))
        };
        assert_eq!(block(&krusty), block(&reference), "`{member}`");
    }
}

/// What the mutable `java.util.Map` members and the bridges do when a Java caller reaches them.
fn box_program(source: &str, expected: &str) -> String {
    format!(
        "{source}
fun outcome(map: Any, name: String, parameters: List<Class<*>>, arguments: List<Any?>): String = try {{
    val result = map.javaClass.getMethod(name, *parameters.toTypedArray())
        .invoke(map, *arguments.toTypedArray())
    \"$name=$result\"
}} catch (e: java.lang.reflect.InvocationTargetException) {{
    \"$name: ${{e.cause!!::class.simpleName}}\"
}}

fun box(): String {{
    val map: Any = Dict()
    val any = Any::class.java
    val outcomes = listOf(
        outcome(map, \"put\", listOf(any, any), listOf(\"k\", 1)),
        outcome(map, \"remove\", listOf(any), listOf(\"k\")),
        outcome(map, \"remove\", listOf(any, any), listOf(\"k\", 1)),
        outcome(map, \"putAll\", listOf(Map::class.java), listOf(mapOf(\"k\" to 1))),
        outcome(map, \"clear\", listOf(), listOf()),
        outcome(map, \"get\", listOf(any), listOf(1)),
        outcome(map, \"keySet\", listOf(), listOf()),
    ).joinToString(\"; \")
    return if (outcomes == \"{expected}\") \"OK\" else outcomes
}}
"
    )
}

#[test]
fn a_map_gets_kotlincs_mutable_map_stubs_and_special_bridges() {
    assert_members_match_kotlinc(
        &string_map(),
        &[
            "public java.lang.Integer put(java.lang.String, int);",
            "public java.lang.Integer remove(java.lang.Object);",
            "public boolean remove(java.lang.Object, java.lang.Object);",
            "public void putAll(java.util.Map<? extends java.lang.String, ? extends java.lang.Integer>);",
            "public void clear();",
            "public final java.lang.Object get(java.lang.Object);",
            "public final java.lang.Integer get(java.lang.Object);",
            "public final java.util.Set<java.lang.String> keySet();",
            "public final java.util.Collection<java.lang.Integer> values();",
            "public final java.util.Set<java.util.Map$Entry<java.lang.String, java.lang.Integer>> entrySet();",
            "public java.lang.Object put(java.lang.Object, java.lang.Object);",
            "public java.lang.Object remove(java.lang.Object);",
        ],
    );
    let expected = "put: UnsupportedOperationException; remove: UnsupportedOperationException; \
        remove: UnsupportedOperationException; putAll: UnsupportedOperationException; \
        clear: UnsupportedOperationException; get=null; keySet=[]";
    common::expect_box_same_as_kotlinc(
        &box_program(&string_map(), expected),
        "read_only_map_face_string",
    );
}

#[test]
fn own_map_members_implement_their_stubs_like_kotlinc() {
    assert_members_match_kotlinc(
        &own_members_map(),
        &[
            "public final java.lang.Integer put(java.lang.String, int);",
            "public final void clear();",
            "public java.lang.Object put(java.lang.Object, java.lang.Object);",
        ],
    );
    let expected = "put=1; remove: UnsupportedOperationException; \
        remove: UnsupportedOperationException; putAll: UnsupportedOperationException; \
        clear=null; get=null; keySet=[]";
    common::expect_box_same_as_kotlinc(
        &box_program(&own_members_map(), expected),
        "read_only_map_face_own_members",
    );
}

#[test]
fn a_narrower_keys_result_gets_kotlincs_special_and_erased_key_set_like_kotlinc() {
    assert_members_match_kotlinc(
        &hash_set_keys_map(),
        &[
            "public final java.util.HashSet<java.lang.String> keySet();",
            "public java.util.Set keySet();",
        ],
    );
}

#[test]
fn an_entry_gets_a_boxed_set_value_stub_like_kotlinc() {
    assert_members_match_kotlinc(
        ENTRY,
        &[
            "public java.lang.Integer setValue(int);",
            "public java.lang.Object setValue(java.lang.Object);",
        ],
    );
}

#[test]
fn a_generic_entry_needs_no_set_value_bridge_like_kotlinc() {
    assert_members_match_kotlinc(GENERIC_ENTRY, &["public V setValue(V);"]);
}

#[test]
fn an_abstract_entry_gets_its_set_value_stub_like_kotlinc() {
    assert_members_match_kotlinc(
        ABSTRACT_ENTRY,
        &[
            "public java.lang.Integer setValue(int);",
            "public java.lang.Object setValue(java.lang.Object);",
        ],
    );
}
