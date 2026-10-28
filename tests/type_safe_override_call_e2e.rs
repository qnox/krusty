//! A call to an override of a type-safe collection declaration (`Map.get`, `Map.containsKey`,
//! `MutableMap.remove`, `Collection.contains`, …) whose parameter narrows the declaration's own
//! type is invoked through the declaration's erased signature, as kotlinc's JVM signature mapper
//! does. The special bridge with that signature then checks the argument and reaches the override.

use super::common;

const SRC: &str = "abstract class Base : AbstractMap<String, Int>() {\n\
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
    }\n\
    class Calls {\n\
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

fn assert_same_method(class: &str, method: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "TypeSafeOverrideCall",
        SRC,
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
fn a_narrowed_collection_override_is_reached_through_its_special_bridge() {
    common::expect_box_same_as_kotlinc(SRC, "TypeSafeOverrideCall");
}
