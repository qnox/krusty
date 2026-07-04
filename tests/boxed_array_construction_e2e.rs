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
    assert_eq!(run(SRC).expect("arrayOfNulls<Int>"), "OK");
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
