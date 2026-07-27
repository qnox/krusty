use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn typed_backing_field_initialized_in_init() {
    const SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
class Holder {\n\
    val numbers: List<Int> field: MutableList<Int>\n\
    init {\n\
        numbers = mutableListOf(1, 2, 3)\n\
    }\n\
}\n\
fun box(): String = when {\n\
    Holder().numbers == listOf(1, 2, 3) -> \"OK\"\n\
    else -> \"Fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("typed field in init"), "OK");
}

#[test]
fn inferred_backing_field_preserves_property_api() {
    const SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
class Inventory {\n\
    val items: List<String>\n\
        field = mutableListOf<String>()\n\
    fun add(item: String) {\n\
        items.add(item)\n\
    }\n\
}\n\
fun view(inventory: Inventory): List<String> = inventory.items\n\
fun box(): String {\n\
    val inventory = Inventory()\n\
    inventory.add(\"OK\")\n\
    return view(inventory)[0]\n\
}\n";
    assert_eq!(run(SRC).expect("inferred field"), "OK");
}

#[test]
fn typed_backing_field_is_visible_inside_owner() {
    const SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
class Holder {\n\
    val value: Any\n\
        field: String = \"OK\"\n\
    fun read(): String = requireString(value)\n\
}\n\
fun requireString(value: String): String = value\n\
fun box(): String = Holder().read()\n";
    assert_eq!(run(SRC).expect("narrowed inside read"), "OK");
}
