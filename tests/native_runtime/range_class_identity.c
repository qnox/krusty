/* A range and a progression are different classes, and each hands out Kotlin's concrete iterator:
   `..` and `until` answer `IntRange` (and its Long, Char, UInt and ULong kin), whose superclass is
   `IntProgression`; `step`, `downTo` and `reversed()` answer the progression class itself, even
   for a step of one; and an iterator is `kotlin.ranges.IntProgressionIterator`, whose superclass is
   the abstract `kotlin.collections.IntIterator` (the unsigned iterators subclass nothing but
   `Any`). Progressions used to be built under the range descriptors, and iterators under the
   abstract `kotlin.collections.*Iterator` ones, so a class literal, a cast or an `is` answered for
   the wrong class.

   The expected classes are Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       fun main() {
           val items = listOf<Any>(1..3, 1..3 step 1, 3 downTo 1, (1..3).reversed(), 1L..3L,
               1L..3L step 2, 'a'..'c', 'c' downTo 'a', 1u..3u, 1u..3u step 2, 1uL..3uL,
               3uL downTo 1uL)
           for (x in items) println("$x | ${x::class.qualifiedName} ${x::class.simpleName} " +
               "super=${x.javaClass.superclass.name}")
           val its = listOf<Any>((1..3).iterator(), (3 downTo 1).iterator(), (1L..3L).iterator(),
               ('a'..'c').iterator(), (1u..3u).iterator(), (1uL..3uL).iterator())
           for (i in its) println("${i::class.qualifiedName} ${i::class.simpleName} " +
               "super=${i.javaClass.superclass.name}")
       }

   whose rows are the table below (`java.lang.Object` is `kotlin.Any` here). */
#include "later_tiers.h"

typedef struct Row {
    KRef value;
    const char *qualified;
    const char *simple;
    const char *parent;
} Row;

static kt_int length_of(const char *text) {
    kt_int length = 0;
    while (text[length] != 0) {
        length++;
    }
    return length;
}

/* The class of `value` names itself `qualified` and `simple`, and its superclass is `parent`. */
static void check(const Row *row) {
    KRef literal = kt_class_of(row->value);
    CHECK(text_is(kt_class_qualified_name(literal), row->qualified, length_of(row->qualified)),
          "a qualified name\n");
    CHECK(text_is(kt_class_simple_name(literal), row->simple, length_of(row->simple)),
          "a simple name\n");
    const KType *parent = type_of(row->value)->super;
    CHECK(parent != NULL && parent->name_length == (uint32_t)length_of(row->parent),
          "a superclass\n");
    for (kt_int at = 0; at < length_of(row->parent); at++) {
        CHECK(parent->name[at] == row->parent[at], "a superclass\n");
    }
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    Row rows[] = {
        {kt_int_range(1, 3), "kotlin.ranges.IntRange", "IntRange", "kotlin.ranges.IntProgression"},
        {kt_range_step(kt_int_range(1, 3), 1), "kotlin.ranges.IntProgression", "IntProgression",
         "kotlin.Any"},
        {kt_int_range_down_to(3, 1), "kotlin.ranges.IntProgression", "IntProgression",
         "kotlin.Any"},
        {kt_range_reversed(kt_int_range(1, 3)), "kotlin.ranges.IntProgression", "IntProgression",
         "kotlin.Any"},
        {kt_long_range(1, 3), "kotlin.ranges.LongRange", "LongRange",
         "kotlin.ranges.LongProgression"},
        {kt_range_step(kt_long_range(1, 3), 2), "kotlin.ranges.LongProgression", "LongProgression",
         "kotlin.Any"},
        {kt_char_range('a', 'c'), "kotlin.ranges.CharRange", "CharRange",
         "kotlin.ranges.CharProgression"},
        {kt_char_range_down_to('c', 'a'), "kotlin.ranges.CharProgression", "CharProgression",
         "kotlin.Any"},
        {kt_uint_range(1, 3), "kotlin.ranges.UIntRange", "UIntRange",
         "kotlin.ranges.UIntProgression"},
        {kt_range_step(kt_uint_range(1, 3), 2), "kotlin.ranges.UIntProgression", "UIntProgression",
         "kotlin.Any"},
        {kt_ulong_range(1, 3), "kotlin.ranges.ULongRange", "ULongRange",
         "kotlin.ranges.ULongProgression"},
        {kt_ulong_range_down_to(3, 1), "kotlin.ranges.ULongProgression", "ULongProgression",
         "kotlin.Any"},
        {kt_range_iterator(kt_int_range(1, 3)), "kotlin.ranges.IntProgressionIterator",
         "IntProgressionIterator", "kotlin.collections.IntIterator"},
        {kt_range_iterator(kt_int_range_down_to(3, 1)), "kotlin.ranges.IntProgressionIterator",
         "IntProgressionIterator", "kotlin.collections.IntIterator"},
        {kt_range_iterator(kt_long_range(1, 3)), "kotlin.ranges.LongProgressionIterator",
         "LongProgressionIterator", "kotlin.collections.LongIterator"},
        {kt_range_iterator(kt_char_range('a', 'c')), "kotlin.ranges.CharProgressionIterator",
         "CharProgressionIterator", "kotlin.collections.CharIterator"},
        {kt_range_iterator(kt_uint_range(1, 3)), "kotlin.ranges.UIntProgressionIterator",
         "UIntProgressionIterator", "kotlin.Any"},
        {kt_range_iterator(kt_ulong_range(1, 3)), "kotlin.ranges.ULongProgressionIterator",
         "ULongProgressionIterator", "kotlin.Any"},
    };
    for (unsigned at = 0; at < sizeof(rows) / sizeof(rows[0]); at++) {
        check(&rows[at]);
    }
    CHECK(kt_pending_exception() == NULL, "building a range raised\n");
    kt_sys_write(1, "OK\n", 3);
}
