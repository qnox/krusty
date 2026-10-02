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

/// Field reads are class-scoped. Inside `Holder`, every value whose static type is exactly
/// `Holder` reads `stamp` with `getfield`, including another instance and a local function or
/// lambda. A `HolderChild` value, a nested or inner class, a subclass reading `inherited`, and a
/// top-level function call the getter.
const OWNER_INSTANCE_SRC: &str = "// LANGUAGE: +ExplicitBackingFields\n\
@JvmInline value class V(val x: Int)\n\
open class Base {\n\
    val inherited: Any\n\
        field = V(7)\n\
    fun onBase(other: Base): Any = other.inherited\n\
    fun onSub(sub: Sub): Any = sub.inherited\n\
}\n\
class Sub : Base() {\n\
    fun read(): Any = inherited\n\
}\n\
open class Holder {\n\
    val stamp: Any\n\
        field = V(3)\n\
    fun bare(): Int = stamp.x\n\
    fun explicit(): Int = this.stamp.x\n\
    fun other(holder: Holder): Any = holder.stamp\n\
    fun child(subclass: HolderChild): Any = subclass.stamp\n\
    fun localFun(): Int {\n\
        fun inner(): Int = this@Holder.stamp.x\n\
        return inner()\n\
    }\n\
    fun lambda(): Int {\n\
        val read: () -> Int = { stamp.x }\n\
        return read()\n\
    }\n\
    fun nested(): Int {\n\
        class Inner {\n\
            fun read(): Int = this@Holder.stamp.x\n\
        }\n\
        return Inner().read()\n\
    }\n\
    inner class Member {\n\
        fun read(): Any = this@Holder.stamp\n\
    }\n\
    fun member(): Any = Member().read()\n\
}\n\
class HolderChild : Holder()\n\
fun outside(holder: Holder): Any = holder.stamp\n\
fun box(): String {\n\
    val holder = Holder()\n\
    if (holder.bare() != 3 || holder.explicit() != 3 || holder.localFun() != 3) return \"owner\"\n\
    if (holder.lambda() != 3 || holder.nested() != 3) return \"closure\"\n\
    if (holder.other(holder) != V(3) || holder.child(HolderChild()) != V(3)) return \"other\"\n\
    if (holder.member() != V(3) || outside(holder) != V(3)) return \"outer\"\n\
    if (Sub().read() != V(7) || Base().onBase(Base()) != V(7) || Base().onSub(Sub()) != V(7)) {\n\
        return \"inherited\"\n\
    }\n\
    return \"OK\"\n\
}\n";

fn assert_owner_instance(class: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "OwnerInstanceBackingField",
        OWNER_INSTANCE_SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &["-XXLanguage:+ExplicitBackingFields".to_string()],
    )
    .expect("reference kotlinc is provisioned");
    let reference = owner_read_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc writes {class}.{member}");
    assert_eq!(
        owner_read_instructions(&built.krusty, member),
        reference,
        "{class}.{member}"
    );
}

/// The outer-instance capture of a local or inner class is a separate spelling (`$this$0` versus
/// `this$0`). The backing-field decision is the rest of the method.
fn owner_read_instructions(disassembly: &str, member: &str) -> Vec<String> {
    common::method_instructions(disassembly, member)
        .into_iter()
        .map(|line| line.replace("$this$0", "OUTER").replace("this$0", "OUTER"))
        .collect()
}

#[test]
fn same_class_instances_read_the_backing_field() {
    assert_owner_instance("Holder", "int bare()");
    assert_owner_instance("Holder", "int explicit()");
    assert_owner_instance("Holder", "java.lang.Object other(Holder)");
    assert_owner_instance("Holder", "int localFun$inner(Holder)");
    assert_owner_instance("Holder", "int lambda$lambda$0(Holder)");
    assert_owner_instance("Base", "java.lang.Object onBase(Base)");
}

#[test]
fn subclass_values_and_nested_classes_call_the_getter() {
    assert_owner_instance("Holder", "java.lang.Object child(HolderChild)");
    assert_owner_instance("Holder$nested$Inner", "int read()");
    assert_owner_instance("Holder$Member", "java.lang.Object read()");
    assert_owner_instance("Sub", "java.lang.Object read()");
    assert_owner_instance("Base", "java.lang.Object onSub(Sub)");
    assert_owner_instance(
        "OwnerInstanceBackingFieldKt",
        "java.lang.Object outside(Holder)",
    );
}

#[test]
fn owner_instance_backing_fields_run() {
    assert_eq!(
        run(OWNER_INSTANCE_SRC).expect("owner-instance backing fields"),
        "OK"
    );
}

#[test]
fn value_class_backing_fields_run() {
    assert_eq!(
        run(VALUE_FIELD_SRC).expect("value-class backing fields"),
        "OK"
    );
}
