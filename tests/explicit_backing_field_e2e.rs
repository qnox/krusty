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
fn inferred_backing_field_is_visible_inside_owner() {
    const SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
class Inventory {\n\
    val items: List<String>\n\
        field = mutableListOf<String>()\n\
    fun add(item: String) {\n\
        items.add(item)\n\
    }\n\
}\n\
fun box(): String {\n\
    val inventory = Inventory()\n\
    inventory.add(\"OK\")\n\
    return if (inventory.items == listOf(\"OK\")) \"OK\" else \"Fail\"\n\
}\n";
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
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

#[test]
fn inferred_top_level_backing_field_is_a_same_file_read_refinement() {
    const SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
val field: String = \"OK\"\n\
val answer: Any\n\
    field = field\n\
fun box(): String = answer\n";

    common::expect_front_end_ok_files_with_stdlib(
        &[SRC],
        "top-level explicit backing-field read refinement",
    );
}
