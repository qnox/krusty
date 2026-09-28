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

/// Inside its owner a property with an explicit backing field reads the field itself, typed as the
/// field: a value-class field is read as its carrier, with no box to cast and unbox again.
const VALUE_FIELD_SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
interface Call { fun call(): Int }\n\
@JvmInline value class Stamp(val x: Int) : Call { override fun call(): Int = x }\n\
@JvmInline value class Rank(val x: Int) : Comparable<Int> {\n\
\x20   override fun compareTo(other: Int): Int = x - other\n\
}\n\
class Holder {\n\
\x20   val stamp: Any\n\
\x20       field = Stamp(1)\n\
\x20   val rank: Comparable<Int>\n\
\x20       field = Rank(2)\n\
\x20   fun read(): Int = stamp.x\n\
\x20   fun twice(): Int = stamp.call() + stamp.x\n\
\x20   fun ranked(): Int = rank.x\n\
}\n\
fun order(c: Comparable<Int>): Int = c.compareTo(2)\n\
fun box(): String {\n\
\x20   val holder = Holder()\n\
\x20   if (holder.read() != 1 || holder.twice() != 2 || holder.ranked() != 2) return \"fail\"\n\
\x20   if (order(holder.rank) != 0 || holder.stamp !is Stamp) return \"fail\"\n\
\x20   return \"OK\"\n\
}\n";

fn assert_same_instructions(class: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "ValueBackingField",
        VALUE_FIELD_SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &["-XXLanguage:+ExplicitBackingFields".to_string()],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc writes {class}.{member}");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference,
        "{class}.{member}"
    );
}

#[test]
fn owner_reads_a_value_class_backing_field_as_its_carrier() {
    assert_same_instructions("Holder", "int read()");
    assert_same_instructions("Holder", "int twice()");
    assert_same_instructions("Holder", "int ranked()");
}

/// The erased `Comparable.compareTo(Object)` bridge of a value class calls the box's
/// `compareTo(int)` entry, never the static `compareTo-impl`.
#[test]
fn a_generic_bridge_of_a_value_class_calls_its_interface_entry() {
    assert_same_instructions("Rank", "int compareTo(java.lang.Object)");
}

#[test]
fn value_class_backing_fields_run() {
    assert_eq!(
        run(VALUE_FIELD_SRC).expect("value-class backing fields"),
        "OK"
    );
}
