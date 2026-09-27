/* A range and a progression are different classes, and each hands out Kotlin's concrete iterator:
   `..` and `until` answer `IntRange` (and its Long, Char, UInt and ULong kin), whose superclass is
   `IntProgression`; `step`, `downTo` and `reversed()` answer the progression class itself, even
   for a step of one; and an iterator is `kotlin.ranges.IntProgressionIterator`, whose superclass is
   the abstract `kotlin.collections.IntIterator` (the unsigned iterators subclass nothing but
   `Any`). Progressions used to be built under the range descriptors, and iterators under the
   abstract `kotlin.collections.*Iterator` ones, so a class literal, a cast or an `is` answered for
   the wrong class.

   The driver prints each object's class -- and, for a range or a progression, its rendering --
   and the harness compares the lines with what `range_class_identity.kt` answers under the
   reference kotlinc. */
#include "transcript.h"

/* `value`'s qualified and simple class names and its superclass's name. */
static void say_class(KRef value) {
    KRef literal = kt_class_of(value);
    say_text(kt_class_qualified_name(literal));
    say(" ");
    say_text(kt_class_simple_name(literal));
    const KType *parent = type_of(value)->super;
    CHECK(parent != NULL, "a range class has no superclass\n");
    say(" super=");
    say_bytes(parent->name, (kt_int)parent->name_length);
    say("\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef items[] = {
        kt_int_range(1, 3),
        kt_range_step(kt_int_range(1, 3), 1),
        kt_int_range_down_to(3, 1),
        kt_range_reversed(kt_int_range(1, 3)),
        kt_long_range(1, 3),
        kt_range_step(kt_long_range(1, 3), 2),
        kt_char_range('a', 'c'),
        kt_char_range_down_to('c', 'a'),
        kt_uint_range(1, 3),
        kt_range_step(kt_uint_range(1, 3), 2),
        kt_ulong_range(1, 3),
        kt_ulong_range_down_to(3, 1),
    };
    for (unsigned at = 0; at < sizeof(items) / sizeof(items[0]); at++) {
        say_value(items[at]);
        say(" | ");
        say_class(items[at]);
    }
    KRef iterators[] = {
        kt_range_iterator(kt_int_range(1, 3)),
        kt_range_iterator(kt_int_range_down_to(3, 1)),
        kt_range_iterator(kt_long_range(1, 3)),
        kt_range_iterator(kt_char_range('a', 'c')),
        kt_range_iterator(kt_uint_range(1, 3)),
        kt_range_iterator(kt_ulong_range(1, 3)),
    };
    for (unsigned at = 0; at < sizeof(iterators) / sizeof(iterators[0]); at++) {
        say_class(iterators[at]);
    }
    CHECK(kt_pending_exception() == NULL, "building a range raised\n");
    kt_sys_write(1, "OK\n", 3);
}
