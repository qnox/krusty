/* The list, walk and array entry points answer what Kotlin answers on the ordinary path: every
   value below is Kotlin's, from this program compiled and run with the reference kotlinc 2.4.10 on
   the JVM (the driver's `T` is `program_collections.h`'s `Tag`):

       class T(val n: Int) {
           override fun equals(other: Any?) = other is T && other.n == n
           override fun hashCode() = n
           override fun toString() = "T$n"
       }
       fun main() {
           val xs = listOf(T(1), T(2), T(3))
           println("$xs ${xs.size} ${xs[1]} ${xs.first()} ${xs.last()} ${xs.indexOf(T(2))} " +
               "${xs.lastIndexOf(T(3))} ${xs.contains(T(4))} ${xs.hashCode()} " +
               "${xs == listOf(T(1), T(2), T(3))} ${xs == listOf(T(1), T(2))}")
           val m = mutableListOf(T(1))
           m.add(T(2)); m.add(0, T(0))
           val old = m.set(1, T(5))
           val removed = m.removeAt(0)
           println("$m $old $removed ${m.remove(T(5))} ${m.remove(T(9))} ${m.size}")
           m.addAll(listOf(T(3), T(4))); m += T(6)
           println("$m ${m == listOf(T(2), T(3), T(4), T(6))} ${m.hashCode()}")
           m.sortWith { a, b -> b.n - a.n }
           println(m)
           m.clear(); println("$m ${m.isEmpty()}")
           val ns = listOf(3, 1, 2)
           println("${ns.map { it * 2 }} ${ns.filter { it > 1 }} ${ns.filterNot { it > 1 }} " +
               "${ns.any { it > 2 }} ${ns.all { it > 0 }} ${ns.none { it > 5 }} ${ns.count()} " +
               "${ns.count { it > 1 }}")
           println("${ns.first { it < 3 }} ${ns.firstOrNull { it > 5 }} ${ns.last { it > 1 }} " +
               "${ns.fold(0) { a, b -> a + b }} ${ns.toList()} ${ns.reversed()} " +
               "${ns.sortedWith { a, b -> a - b }}")
           val seen = StringBuilder(); ns.forEachIndexed { i, v -> seen.append("$i:$v,") }
           println("$seen ${ns.joinToString()} ${ns + 4} ${ns + listOf(5, 6)} ${ns.sumOf { it }} " +
               "${ns.sumOf { it.toLong() * 3000000000L }} ${ns.sumOf { it / 2.0 }}")
           println("${ns.withIndex().toList()} ${ns.isEmpty()} ${ns.isNotEmpty()} " +
               "${listOf<Int>().any()}")
           val iv = IndexedValue(1, T(2))
           println("$iv ${iv.hashCode()} ${iv == IndexedValue(1, T(2))} " +
               "${iv == IndexedValue(2, T(2))}")
           val ia = intArrayOf(1, 2, 3)
           println("${ia.toList()} ${ia.reversed()} ${ia.reversedArray().contentToString()} " +
               "${ia.isEmpty()} ${intArrayOf().isNotEmpty()}")
           val ta = arrayOf(T(1), T(2))
           println("${ta.contentEquals(arrayOf(T(1), T(2)))} ${ta.contentHashCode()} " +
               "${ta.contentToString()} ${ta.toList()} " +
               "${listOf(1, 2).toTypedArray().contentToString()}")
           println("${(1..4 step 2).map { it }} ${(3 downTo 1).toList()} " +
               "${('a'..'c').joinToString()} ${(1..3).count()} ${"héllo".map { it }} " +
               "${"ab".toList()}")
           println("${listOf(T(1)) + T(2)} ${listOf(1, null, 3)}")
       }

   whose output is recorded beside each check below. */
#include "program_collections.h"

#define RENDERS(value, literal) text_is(kt_to_string(value), literal, sizeof(literal) - 1)
#define ELEMENT(array, at) (((KRef *)((KArray *)(array) + 1))[at])

static KRef array_of(KRef *elements, kt_int count) {
    KRef array = kt_array_new(&kt_type_array, count);
    for (kt_int at = 0; at < count; at++) {
        ELEMENT(array, at) = elements[at];
    }
    return array;
}

static KRef ints(kt_int a, kt_int b, kt_int c) {
    KRef elements[] = {kt_box_int(a), kt_box_int(b), kt_box_int(c)};
    return kt_list_of(array_of(elements, 3));
}

static kt_int n_of(KRef value) { return kt_unbox_int(value); }

static KRef times_two(KRef self, KRef x) { (void)self; return kt_box_int(n_of(x) * 2); }
static KRef above_one(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) > 1); }
static KRef above_two(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) > 2); }
static KRef above_zero(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) > 0); }
static KRef above_five(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) > 5); }
static KRef below_three(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) < 3); }
static KRef plus(KRef self, KRef a, KRef b) { (void)self; return kt_box_int(n_of(a) + n_of(b)); }
static KRef ascending(KRef self, KRef a, KRef b) {
    (void)self;
    return kt_box_int(n_of(a) - n_of(b));
}
static KRef identity(KRef self, KRef x) { (void)self; return x; }
static KRef as_long(KRef self, KRef x) { (void)self; return kt_box_long(n_of(x) * 3000000000LL); }
static KRef halved(KRef self, KRef x) { (void)self; return kt_box_double(n_of(x) / 2.0); }

static KRef tags_descending(KRef self, KRef a, KRef b) {
    (void)self;
    return kt_box_int(((const Tag *)b)->n - ((const Tag *)a)->n);
}

static char seen[32];
static kt_int seen_length;

static KRef record(KRef self, KRef index, KRef value) {
    (void)self;
    seen[seen_length++] = (char)('0' + n_of(index));
    seen[seen_length++] = ':';
    seen[seen_length++] = (char)('0' + n_of(value));
    seen[seen_length++] = ',';
    return NULL;
}

FUNCTION_VALUE(f_times_two, times_two)
FUNCTION_VALUE(f_above_one, above_one)
FUNCTION_VALUE(f_above_two, above_two)
FUNCTION_VALUE(f_above_zero, above_zero)
FUNCTION_VALUE(f_above_five, above_five)
FUNCTION_VALUE(f_below_three, below_three)
FUNCTION_VALUE(f_plus, plus)
FUNCTION_VALUE(f_ascending, ascending)
FUNCTION_VALUE(f_identity, identity)
FUNCTION_VALUE(f_as_long, as_long)
FUNCTION_VALUE(f_halved, halved)
FUNCTION_VALUE(f_tags_descending, tags_descending)
FUNCTION_VALUE(f_record, record)

void kt_program_entry(void) {
    PROGRAM_BEGIN();

    /* [T1, T2, T3] 3 T2 T1 T3 1 2 false 30817 true false */
    KRef t123[] = {tag(1, ""), tag(2, ""), tag(3, "")};
    KRef xs = kt_list_of(array_of(t123, 3));
    CHECK(RENDERS(xs, "[T1, T2, T3]") && kt_list_size(xs) == 3, "listOf(T(1), T(2), T(3))\n");
    CHECK(((const Tag *)kt_list_get(xs, 1))->n == 2 && ((const Tag *)kt_list_first(xs))->n == 1 &&
              ((const Tag *)kt_list_last(xs))->n == 3,
          "get, first, last\n");
    CHECK(kt_list_index_of(xs, tag(2, "")) == 1 && kt_list_last_index_of(xs, tag(3, "")) == 2 &&
              !kt_list_contains(xs, tag(4, "")),
          "indexOf, lastIndexOf, contains\n");
    CHECK(kt_hash_code(xs) == 30817, "hashCode\n");
    KRef t12[] = {tag(1, ""), tag(2, "")};
    CHECK(kt_equals(xs, kt_list_of(array_of(t123, 3))) &&
              !kt_equals(xs, kt_list_of(array_of(t12, 2))),
          "equals\n");

    /* [T5, T2] T1 T0 true false 1 */
    KRef t1[] = {tag(1, "")};
    KRef m = kt_mutable_list_of(array_of(t1, 1));
    kt_mutable_list_add(m, tag(2, ""));
    kt_mutable_list_add_at(m, 0, tag(0, ""));
    KRef old = kt_mutable_list_set(m, 1, tag(5, ""));
    KRef removed = kt_mutable_list_remove_at(m, 0);
    CHECK(RENDERS(m, "[T5, T2]") && ((const Tag *)old)->n == 1 && ((const Tag *)removed)->n == 0,
          "add, add(0, e), set, removeAt\n");
    CHECK(kt_mutable_list_remove(m, tag(5, "")) && !kt_mutable_list_remove(m, tag(9, "")) &&
              kt_list_size(m) == 1,
          "remove\n");

    /* [T2, T3, T4, T6] true 986116 */
    KRef t34[] = {tag(3, ""), tag(4, "")};
    kt_mutable_list_add_all(m, kt_list_of(array_of(t34, 2)));
    kt_mutable_list_plus_assign(m, tag(6, ""));
    KRef t2346[] = {tag(2, ""), tag(3, ""), tag(4, ""), tag(6, "")};
    CHECK(RENDERS(m, "[T2, T3, T4, T6]") && kt_equals(m, kt_list_of(array_of(t2346, 4))) &&
              kt_hash_code(m) == 986116,
          "addAll, +=, equals, hashCode\n");
    /* [T6, T4, T3, T2] */
    kt_list_sort_with(m, f_tags_descending);
    CHECK(RENDERS(m, "[T6, T4, T3, T2]"), "sortWith\n");
    /* [] true */
    kt_mutable_list_clear(m);
    CHECK(RENDERS(m, "[]") && kt_list_is_empty(m), "clear\n");

    /* [6, 2, 4] [3, 2] [1] true true true 3 2 */
    KRef ns = ints(3, 1, 2);
    CHECK(RENDERS(kt_iterable_map(ns, f_times_two), "[6, 2, 4]"), "map\n");
    CHECK(RENDERS(kt_iterable_filter(ns, f_above_one), "[3, 2]"), "filter\n");
    CHECK(RENDERS(kt_iterable_filter_not(ns, f_above_one), "[1]"), "filterNot\n");
    CHECK(kt_iterable_any(ns, f_above_two) && kt_iterable_all(ns, f_above_zero) &&
              kt_iterable_none(ns, f_above_five),
          "any, all, none\n");
    CHECK(kt_iterable_count(ns) == 3 && kt_iterable_count_matching(ns, f_above_one) == 2,
          "count\n");

    /* 1 null 2 6 [3, 1, 2] [2, 1, 3] [1, 2, 3] */
    CHECK(n_of(kt_iterable_first_matching(ns, f_below_three)) == 1 &&
              kt_iterable_first_or_null(ns, f_above_five) == NULL &&
              n_of(kt_iterable_last_matching(ns, f_above_one)) == 2,
          "first, firstOrNull, last\n");
    CHECK(n_of(kt_iterable_fold(ns, kt_box_int(0), f_plus)) == 6, "fold\n");
    CHECK(RENDERS(kt_iterable_to_list(ns), "[3, 1, 2]") &&
              RENDERS(kt_iterable_reversed(ns), "[2, 1, 3]") &&
              RENDERS(kt_iterable_sorted_with(ns, f_ascending), "[1, 2, 3]"),
          "toList, reversed, sortedWith\n");

    /* 0:3,1:1,2:2, 3, 1, 2 [3, 1, 2, 4] [3, 1, 2, 5, 6] 6 18000000000 3.0 */
    kt_iterable_for_each_indexed(ns, f_record);
    CHECK(seen_length == 12 && text_is(kt_string_utf8(seen, seen_length), "0:3,1:1,2:2,", 12),
          "forEachIndexed\n");
    CHECK(text_is(kt_iterable_join_to_string(ns), "3, 1, 2", 7), "joinToString\n");
    KRef five_six[] = {kt_box_int(5), kt_box_int(6)};
    CHECK(RENDERS(kt_iterable_plus_element(ns, kt_box_int(4)), "[3, 1, 2, 4]") &&
              RENDERS(kt_iterable_plus_all(ns, kt_list_of(array_of(five_six, 2))),
                      "[3, 1, 2, 5, 6]"),
          "plus\n");
    CHECK(kt_iterable_sum_of_int(ns, f_identity) == 6 &&
              kt_iterable_sum_of_long(ns, f_as_long) == 18000000000LL &&
              kt_iterable_sum_of_double(ns, f_halved) == 3.0,
          "sumOf\n");

    /* [IndexedValue(index=0, value=3), ...] false true false */
    static const char indexed[] = "[IndexedValue(index=0, value=3), "
                                  "IndexedValue(index=1, value=1), "
                                  "IndexedValue(index=2, value=2)]";
    CHECK(text_is(kt_to_string(kt_iterable_to_list(kt_iterable_with_index(ns))), indexed,
                  sizeof(indexed) - 1),
          "withIndex\n");
    CHECK(!kt_iterable_is_empty(ns) && kt_iterable_is_not_empty(ns) &&
              !kt_iterable_is_not_empty(kt_list_empty()),
          "isEmpty, isNotEmpty, any()\n");

    /* IndexedValue(index=1, value=T2) 33 true false */
    KRef iv = kt_indexed_value(1, tag(2, ""));
    CHECK(RENDERS(iv, "IndexedValue(index=1, value=T2)") && kt_hash_code(iv) == 33 &&
              kt_equals(iv, kt_indexed_value(1, tag(2, ""))) &&
              !kt_equals(iv, kt_indexed_value(2, tag(2, ""))),
          "IndexedValue\n");

    /* [1, 2, 3] [3, 2, 1] [3, 2, 1] false false */
    KRef ia = kt_array_new(&kt_type_int_array, 3);
    for (kt_int at = 0; at < 3; at++) {
        ((kt_int *)((KArray *)ia + 1))[at] = at + 1;
    }
    CHECK(RENDERS(kt_array_to_list(ia), "[1, 2, 3]") &&
              RENDERS(kt_array_reversed(ia), "[3, 2, 1]") &&
              text_is(kt_array_content_to_string(kt_array_reversed_array(ia)), "[3, 2, 1]", 9) &&
              !kt_array_is_empty(ia) && !kt_array_is_not_empty(kt_array_new(&kt_type_int_array, 0)),
          "IntArray toList, reversed, reversedArray, isEmpty\n");

    /* true 994 [T1, T2] [T1, T2] [1, 2] */
    KRef ta = array_of(t12, 2);
    KRef t12_again[] = {tag(1, ""), tag(2, "")};
    CHECK(kt_array_content_equals(ta, array_of(t12_again, 2)) &&
              kt_array_content_hash_code(ta) == 994 &&
              text_is(kt_array_content_to_string(ta), "[T1, T2]", 8) &&
              RENDERS(kt_array_to_list(ta), "[T1, T2]"),
          "contentEquals, contentHashCode, contentToString, toList\n");
    KRef one_two[] = {kt_box_int(1), kt_box_int(2)};
    CHECK(text_is(kt_array_content_to_string(
                      kt_iterable_to_typed_array(kt_list_of(array_of(one_two, 2)))),
                  "[1, 2]", 6),
          "toTypedArray\n");

    /* [1, 3] [3, 2, 1] a, b, c 3 [h, é, l, l, o] [a, b] */
    CHECK(RENDERS(kt_iterable_map(kt_range_step(kt_int_range(1, 4), 2), f_identity), "[1, 3]") &&
              RENDERS(kt_iterable_to_list(kt_int_range_down_to(3, 1)), "[3, 2, 1]") &&
              text_is(kt_iterable_join_to_string(kt_char_range('a', 'c')), "a, b, c", 7) &&
              kt_iterable_count(kt_int_range(1, 3)) == 3,
          "ranges\n");
    CHECK(RENDERS(kt_iterable_map(kt_string_utf8("h\xC3\xA9llo", 6), f_identity),
                  "[h, \xC3\xA9, l, l, o]") &&
              RENDERS(kt_iterable_to_list(kt_string_utf8("ab", 2)), "[a, b]"),
          "strings\n");

    /* [T1, T2] [1, null, 3] */
    CHECK(RENDERS(kt_iterable_plus_element(kt_list_of(array_of(t1, 1)), tag(2, "")), "[T1, T2]"),
          "listOf(T(1)) + T(2)\n");
    KRef with_null[] = {kt_box_int(1), NULL, kt_box_int(3)};
    CHECK(RENDERS(kt_list_of(array_of(with_null, 3)), "[1, null, 3]"), "a null element\n");

    CHECK(kt_pending_exception() == NULL, "an ordinary call raised\n");
    kt_sys_write(1, "OK\n", 3);
}
