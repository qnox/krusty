//! Boxed `Array<T>` construction and use. `Array<Int>` is `Integer[]` (boxed), distinct from `IntArray`
//! (`[I`). Covers `arrayOf`, `Array(n){}`, declared `Array<T>` (param/return/getter/delegate), `.size`,
//! `for`, index get/set, and an explicit type argument. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn arrayof_and_size_and_for_and_index() {
    const SRC: &str = "fun box(): String {\n\
    val a = arrayOf(1, 2, 3)\n\
    val b = Array(3) { it * 10 }\n\
    if (a.size != 3 || b.size != 3) return \"size\"\n\
    if (a[2] != 3 || b[1] != 10) return \"index\"\n\
    b[0] = 99\n\
    var sum = 0\n\
    for (x in a) sum += x\n\
    return if (sum == 6 && b[0] == 99) \"OK\" else \"fail sum=$sum b0=${b[0]}\"\n\
}\n";
    assert_eq!(run(SRC).expect("arrayOf/Array(n)"), "OK");
}

#[test]
fn declared_array_param_return_and_compound_assign() {
    const SRC: &str = "fun mk(): Array<Int> = Array(2) { it }\n\
fun sumOf(a: Array<Int>): Int { var s = 0; for (x in a) s += x; return s }\n\
fun box(): String {\n\
    val a: Array<Int> = mk()\n\
    a[1] += 40\n\
    a[0]++\n\
    return if (sumOf(a) == 42) \"OK\" else \"fail ${sumOf(a)}\"\n\
}\n";
    assert_eq!(run(SRC).expect("declared Array<Int>"), "OK");
}

#[test]
fn boxed_array_property_getter() {
    const SRC: &str = "class C {\n\
    val arr: Array<Int> get() = Array(4) { it }\n\
}\n\
fun box(): String {\n\
    val c = C()\n\
    return if (c.arr.size == 4 && c.arr[3] == 3) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("getter Array<Int>"), "OK");
}

#[test]
fn array_of_nulls_primitive() {
    // `arrayOfNulls<Int>(n)` is `Array<Int?>` = `Integer[]` of nulls; the element is nullable.
    const SRC: &str = "fun box(): String {\n\
    val a = arrayOfNulls<Int>(3)\n\
    if (a.size != 3 || a[0] != null) return \"f1\"\n\
    a[0] = 7\n\
    return if (a[0] == 7 && a[1] == null) \"OK\" else \"f2\"\n\
}\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "ArrayOfNullsPrimitive"),
        "OK"
    );
}

#[test]
fn explicit_type_argument_byte() {
    const SRC: &str = "fun box(): String {\n\
    val a = arrayOf<Byte>(1, 2)\n\
    a[0]++\n\
    return if (a[0] == 2.toByte() && a[1] == 2.toByte()) \"OK\" else \"fail ${a[0]},${a[1]}\"\n\
}\n";
    assert_eq!(run(SRC).expect("arrayOf<Byte>"), "OK");
}

#[test]
fn jvm_array_clone_has_exact_array_type_and_object_realization() {
    const SRC: &str = "fun acceptsCloneable(value: Cloneable): Boolean = value is Cloneable\n\
fun acceptsSerializable(value: java.io.Serializable): Boolean = value is java.io.Serializable\n\
fun box(): String {\n\
    val strings = arrayOf(\"OK\")\n\
    val ints = intArrayOf(1, 2)\n\
    val stringsCopy: Array<String> = strings.clone()\n\
    val intsCopy: IntArray = ints.clone()\n\
    if (stringsCopy === strings || intsCopy === ints) return \"identity\"\n\
    if (stringsCopy[0] != \"OK\" || intsCopy[1] != 2) return \"content\"\n\
    if (!acceptsCloneable(strings) || !acceptsCloneable(ints)) return \"cloneable\"\n\
    if (!acceptsSerializable(strings) || !acceptsSerializable(ints)) return \"serializable\"\n\
    return \"OK\"\n\
}\n";
    common::expect_true_e2e(
        "jvm_array_clone_has_exact_array_type_and_object_realization",
        SRC,
        &[],
    );
    assert_eq!(run(SRC).expect("JVM Array.clone"), "OK");
}

#[test]
fn context_typed_empty_arrayof_compiles_and_runs() {
    const SRC: &str = "val top: Array<String> = arrayOf()\n\
class Holder {\n\
    val member: Array<String> = arrayOf()\n\
    val getter: Array<String> get() = arrayOf()\n\
    fun block(): Array<String> { return arrayOf() }\n\
}\n\
fun accept(values: Array<String>): Array<String> = values\n\
fun emptyStrings(): Array<String> = arrayOf()\n\
fun box(): String {\n\
    val ints: Array<Int> = arrayOf()\n\
    val nullableInts: Array<Int?> = arrayOf(42)\n\
    val strings = emptyStrings()\n\
    var assigned: Array<String> = arrayOf(\"x\")\n\
    assigned = arrayOf()\n\
    val argument = accept(arrayOf())\n\
    val holder = Holder()\n\
    if (!ints.isEmpty() || !strings.isEmpty() || !top.isEmpty()) return \"size\"\n\
    if (!assigned.isEmpty() || !argument.isEmpty()) return \"context\"\n\
    if (!holder.member.isEmpty() || !holder.getter.isEmpty() || !holder.block().isEmpty()) return \"member\"\n\
    if (ints.javaClass.componentType.name != \"java.lang.Integer\") return \"ints\"\n\
    if (nullableInts.javaClass.componentType.name != \"java.lang.Integer\") return \"nullable ints\"\n\
    if (strings.javaClass.componentType.name != \"java.lang.String\") return \"strings\"\n\
    if (top.javaClass.componentType.name != \"java.lang.String\") return \"top\"\n\
    if (assigned.javaClass.componentType.name != \"java.lang.String\") return \"assigned\"\n\
    if (argument.javaClass.componentType.name != \"java.lang.String\") return \"argument\"\n\
    if (holder.member.javaClass.componentType.name != \"java.lang.String\") return \"member type\"\n\
    if (holder.getter.javaClass.componentType.name != \"java.lang.String\") return \"getter type\"\n\
    if (holder.block().javaClass.componentType.name != \"java.lang.String\") return \"block type\"\n\
    return \"OK\"\n\
}\n";
    common::expect_true_e2e("context_typed_empty_arrayof_compiles_and_runs", SRC, &[]);
    assert_eq!(run(SRC).expect("context-typed empty arrayOf"), "OK");
}

#[test]
fn inline_initializers_return_from_the_enclosing_function() {
    const SRC: &str = "fun objectArray() {\n\
    Array<String>(5) { i -> if (i == 3) return; i.toString() }\n\
    throw AssertionError(\"object initializer did not return\")\n\
}\n\
fun primitiveArray() {\n\
    IntArray(5) { i -> if (i == 3) return; i }\n\
    throw AssertionError(\"primitive initializer did not return\")\n\
}\n\
fun box(): String { objectArray(); primitiveArray(); return \"OK\" }\n";
    assert_eq!(run(SRC).expect("inline array initializers"), "OK");
}
