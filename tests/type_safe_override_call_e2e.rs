//! A call to an override of a type-safe collection declaration (`Map.get`, `Map.containsKey`,
//! `MutableMap.remove`, `Collection.contains`, …) whose parameter narrows the declaration's own
//! type is invoked through the declaration's erased signature, as kotlinc's JVM signature mapper
//! does. The special bridge with that signature then checks the argument and reaches the override.

use super::common;

const DECLARATIONS: &str = "abstract class Base : AbstractMap<String, Int>() {\n\
    \x20   override val entries: Set<Map.Entry<String, Int>> get() = emptySet()\n\
    \x20   override fun get(key: String): Int? = key.length\n\
    \x20   override fun containsKey(key: String): Boolean = key == \"k\"\n\
    \x20   fun own(): Int? = get(\"own\")\n\
    }\n\
    class Sub : Base()\n\
    class Words : AbstractCollection<String>() {\n\
    \x20   override val size: Int get() = 1\n\
    \x20   override fun iterator(): Iterator<String> = listOf(\"w\").iterator()\n\
    \x20   override fun contains(element: String): Boolean = element == \"w\"\n\
    }\n\
    class Removing : java.util.HashMap<String, Int>() {\n\
    \x20   override fun remove(key: String): Int? = key.length\n\
    }\n\
    class Numbers : java.util.HashSet<Int>() {\n\
    \x20   override fun remove(element: Int): Boolean = super.remove(element)\n\
    }\n";

const CALLS: &str = "class Calls {\n\
    \x20   fun read(map: Sub): Int? = map[\"abc\"]\n\
    \x20   fun has(map: Base): Boolean = map.containsKey(\"k\")\n\
    \x20   fun contains(words: Words): Boolean = \"w\" in words\n\
    \x20   fun remove(map: Removing): Int? = map.remove(\"four\")\n\
    \x20   fun removeNumber(numbers: Numbers): Boolean = numbers.remove(1)\n\
    }\n\
    fun box(): String {\n\
    \x20   val calls = Calls()\n\
    \x20   val map = Sub()\n\
    \x20   if (calls.read(map) != 3 || map.own() != 3) return \"fail get\"\n\
    \x20   if (!calls.has(map) || (map as Map<Any, Any>).containsKey(1)) return \"fail containsKey\"\n\
    \x20   if (!calls.contains(Words())) return \"fail contains\"\n\
    \x20   val numbers = Numbers()\n\
    \x20   numbers.add(1)\n\
    \x20   if (!calls.removeNumber(numbers) || numbers.contains(1)) return \"fail remove element\"\n\
    \x20   return if (calls.remove(Removing()) == 4) \"OK\" else \"fail remove\"\n\
    }\n";

/// A dependency module: a delegated map and a narrowed `AbstractMap` hierarchy whose overrides the
/// consumer calls through the erased declaration signature.
const LIBRARY: &str = "class Values(private val source: Map<String, Int>) : Map<String, Int> by source\n\
    abstract class Lengths : AbstractMap<String, Int>() {\n\
    \x20   override val entries: Set<Map.Entry<String, Int>> get() = throw UnsupportedOperationException()\n\
    \x20   override fun get(key: String): Int? = key.length\n\
    \x20   override fun containsKey(key: String): Boolean = key.isNotEmpty()\n\
    }\n\
    class Words : Lengths()\n";

const CONSUMER: &str = "fun read(values: Values): Int? = values[\"one\"]\n\
    fun length(words: Words): Int? = words[\"four\"]\n\
    fun has(words: Lengths): Boolean = words.containsKey(\"k\")\n\
    fun box(): String {\n\
    \x20   if (read(Values(mapOf(\"one\" to 1))) != 1) return \"delegated get\"\n\
    \x20   if (length(Words()) != 4) return \"overridden get\"\n\
    \x20   if (!has(Words()) || (Words() as Map<Any, Any>).containsKey(1)) return \"containsKey\"\n\
    \x20   return \"OK\"\n\
    }\n";

/// The classes whose code calls a narrowed override.
const AFFECTED: [&str; 2] = ["Calls", "Base"];

fn single_file() -> String {
    format!("{DECLARATIONS}{CALLS}")
}

fn assert_same_method(class: &str, method: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "TypeSafeOverrideCall",
        &single_file(),
        class,
        &[common::stdlib_jar(), common::jdk_modules()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, method);
    assert!(!reference.is_empty(), "kotlinc writes {class}.{method}");
    assert_eq!(
        common::method_instructions(&built.krusty, method),
        reference,
        "{class}.{method}"
    );
}

#[test]
fn a_call_to_a_narrowed_collection_override_uses_the_erased_signature_like_kotlinc() {
    for method in [
        "java.lang.Integer read(Sub)",
        "boolean has(Base)",
        "boolean contains(Words)",
        "java.lang.Integer remove(Removing)",
        "boolean removeNumber(Numbers)",
    ] {
        assert_same_method("Calls", method);
    }
    assert_same_method("Base", "java.lang.Integer own()");
}

#[test]
fn classes_calling_a_narrowed_collection_override_are_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "TypeSafeOverrideCall",
        &single_file(),
        &AFFECTED,
    );
}

/// An override declared in another source file of the module is called through the same erased
/// signature, with a primitive argument boxed for it.
#[test]
fn a_cross_file_call_to_a_narrowed_collection_override_is_byte_identical_to_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Declarations.kt", DECLARATIONS),
        ("Calls.kt", CALLS),
    ]);
    let differences = classes
        .differences()
        .into_iter()
        .filter(|difference| {
            AFFECTED
                .iter()
                .any(|class| difference.starts_with(&format!("{class} ")))
        })
        .collect::<Vec<_>>();
    assert_eq!(differences, Vec::<String>::new());
}

#[test]
fn a_narrowed_collection_override_is_reached_through_its_special_bridge() {
    common::expect_box_same_as_kotlinc(&single_file(), "TypeSafeOverrideCall");
    common::expect_box_ok_files_with_stdlib(
        &[("Declarations.kt", DECLARATIONS), ("Calls.kt", CALLS)],
        "TypeSafeOverrideCallFiles",
    );
}

/// A consumer of a dependency module's narrowed overrides and delegated map, the dependency built
/// by krusty itself.
#[test]
fn a_dependency_override_is_called_through_the_erased_signature() {
    assert_eq!(
        common::expect_box_run_against("TypeSafeOverrideCallLibrary", LIBRARY, CONSUMER).as_deref(),
        Some("OK")
    );
}
