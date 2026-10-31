//! A bound reference to an array classifier's own member (`array::get`). The array classes have no
//! JVM class of their own, so kotlinc reflects the member on the JVM array class (`[I`, or the
//! erased `[Ljava/lang/Object;` for `Array<T>`) under the signature its declaration maps to
//! (`get(I)I`, `get(I)Ljava/lang/Object;`), and its `invoke` reads the element with the array
//! instruction.
use super::common;

const SOURCE: &str = "fun ints(values: IntArray): (Int) -> Int = values::get\n\
    fun strings(values: Array<String>): (Int) -> String = values::get\n\
    fun boxed(values: Array<Int>): (Int) -> Int = values::get\n\
    fun read(values: Array<Int>): Int = values.get(1)\n\
    fun box(): String {\n\
    \x20   if (ints(IntArray(2) { it + 1 })(1) != 2) return \"Fail ints\"\n\
    \x20   if (boxed(Array(3) { it + 1 })(1) != 2) return \"Fail boxed\"\n\
    \x20   if (read(Array(3) { it + 1 }) != 2) return \"Fail read\"\n\
    \x20   return strings(Array(1) { \"OK\" })(0)\n\
    }\n";

#[test]
fn a_bound_array_member_reference_is_reflected_on_the_jvm_array_class() {
    common::assert_class_matches_kotlinc("ArrayMemberRef", SOURCE, "ArrayMemberRefKt$ints$1");
    common::assert_class_matches_kotlinc("ArrayMemberRef", SOURCE, "ArrayMemberRefKt$strings$1");
    common::expect_box_same_as_kotlinc(SOURCE, "ArrayMemberRefRun");
}

/// `Array<T>.get` returns the element a reference array stores: kotlinc unboxes the `Integer` its
/// `aaload` leaves straight to `int`, without first erasing it to `Object`.
#[test]
fn a_generic_array_read_unboxes_the_stored_element() {
    common::assert_class_code_matches_kotlinc("ArrayMemberRead", SOURCE, "ArrayMemberReadKt");
}

/// Explicit calls of the array classifiers' own members. The provider attaches the array operation
/// to the exact `get`/`set`/`size` declarations, so each call is the element access or
/// `arraylength` kotlinc emits; `iterator()` calls the static helper declared for the array class.
const CALLS: &str = "fun write(values: Array<Int>) { values.set(0, 5) }\n\
    fun prim(values: LongArray): Long { values.set(0, 7L); return values.get(0) + values.size }\n\
    fun first(values: IntArray): Int = values.iterator().next()\n\
    fun firstName(values: Array<String>): String = values.iterator().next()\n\
    fun box(): String {\n\
    \x20   val boxed = Array(2) { 0 }\n\
    \x20   write(boxed)\n\
    \x20   if (boxed[0] != 5) return \"Fail write\"\n\
    \x20   if (prim(LongArray(2)) != 9L) return \"Fail prim\"\n\
    \x20   if (first(intArrayOf(3, 4)) != 3) return \"Fail first\"\n\
    \x20   return firstName(arrayOf(\"OK\"))\n\
    }\n";

#[test]
fn explicit_array_member_calls_are_array_operations() {
    common::assert_class_code_matches_kotlinc("ArrayMemberCalls", CALLS, "ArrayMemberCallsKt");
    common::expect_box_same_as_kotlinc(CALLS, "ArrayMemberCallsRun");
}

/// A reference to `IntArray::size` reads the length through that same `size` operation.
const SIZE_REFERENCE: &str = "fun length(values: IntArray): Int {\n\
    \x20   val size: (IntArray) -> Int = IntArray::size\n\
    \x20   return size(values)\n\
    }\n\
    fun box(): String = if (length(IntArray(4)) == 4) \"OK\" else \"Fail\"\n";

#[test]
fn an_array_size_reference_reads_the_array_length() {
    common::expect_box_same_as_kotlinc(SIZE_REFERENCE, "ArraySizeReference");
}

/// JVM `Array<T>.clone()` returns the receiver's own array type, nullable element included:
/// `Array<String?>.clone()` is an `Array<String?>`, never `Array<String>`.
const NULLABLE_CLONE: &str = "class Reader(val names: Array<String?>) {\n\
    \x20   constructor(copyFrom: Reader) : this(arrayOfNulls(1)) {\n\
    \x20       var copied: Array<String?> = arrayOfNulls(0)\n\
    \x20       copied = copyFrom.names.clone()\n\
    \x20       copied[0] = null\n\
    \x20       names[0] = copyFrom.names[0]\n\
    \x20   }\n\
    }\n\
    fun copy(values: Array<String?>): Array<String?> = values.clone()\n\
    fun box(): String {\n\
    \x20   val source = Reader(arrayOf(\"OK\", null))\n\
    \x20   val cloned = copy(source.names)\n\
    \x20   if (cloned === source.names || cloned[1] != null) return \"Fail clone\"\n\
    \x20   return Reader(source).names[0] ?: \"Fail copy\"\n\
    }\n";

#[test]
fn a_nullable_element_array_clone_keeps_its_element_type() {
    common::expect_box_same_as_kotlinc(NULLABLE_CLONE, "ArrayNullableCloneRun");
}
