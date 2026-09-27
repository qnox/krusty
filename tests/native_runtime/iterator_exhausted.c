/* `next()` on an exhausted iterator raises what Kotlin raises there, catchably, through both the
   general protocol (`kt_iterator_next`, a boxed element) and the narrow one
   (`kt_range_iterator_next`, the element's bits):

   - an array's, Kotlin/Native's `ArrayIterator` and `IntArrayIterator` family:
     `NoSuchElementException` whose message is the index asked for (`"1"`);
   - a range's or progression's, a list's and `withIndex()`'s: `NoSuchElementException()`;
   - a `String`'s or a `StringBuilder`'s: whatever reading the text past its end raises, because
     Kotlin's `CharSequence.iterator()` answers `get(index++)` with no check of its own — this
     runtime's `IndexOutOfBoundsException("Index 1 out of bounds for length 1")`.

   The general protocol used to answer a string's exhaustion with `NoSuchElementException`, and
   the narrow one ended the program for an exhausted array, string or range.

   The driver prints what each `next()` raised, and the harness compares the lines with what
   `iterator_exhausted.kt` answers under the reference kotlinc. An array iterator's message is
   platform-defined: the JVM's is `Index 1 out of bounds for length 1`, while Kotlin/Native 2.4.10's
   `IntArrayIterator.nextInt` and `ArrayIterator.next` (the distribution's linux_x64 stdlib cache)
   build it as `index.toString()`, and this runtime is the native one; so the transcript names only
   an array's exception class, and the driver pins its message. */
#include "collections_later_tiers.h"
#include "transcript.h"

/* The line for one `next()` past the end: what it raised, and, unless `message` is NULL, the
   message too. An array's message is Kotlin/Native's, not the JVM's, so it is `array_message`
   instead: pinned here, and left off the line. */
static void say_exhausted(const char *label, const char *protocol, const char *array_message) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    CHECK(thrown != NULL, "next() past the end raised nothing\n");
    say(label);
    say(protocol);
    say(" threw ");
    say_simple_name(type_of(thrown));
    if (array_message == NULL) {
        say(": ");
        say_value(kt_throwable_message(thrown));
    } else {
        kt_int length = 0;
        while (array_message[length] != 0) {
            length++;
        }
        CHECK(text_is(kt_throwable_message(thrown), array_message, length),
              "an exhausted array iterator's message is not Kotlin/Native's\n");
    }
    say("\n");
}

/* Walk `iterable` past its `elements` elements through the general protocol, then again through
   the narrow one when `narrow` says it has one. */
static void exhaust(const char *label, KRef iterable, kt_int elements, kt_boolean narrow,
                    const char *array_message) {
    KRef walk = kt_iterable_iterator(iterable);
    for (kt_int at = 0; at < elements; at++) {
        (void)kt_iterator_next(walk);
    }
    CHECK(kt_pending_exception() == NULL, "an element before the end raised\n");
    CHECK(!kt_iterator_has_next(walk), "an exhausted iterator has a next element\n");
    (void)kt_iterator_next(walk);
    say_exhausted(label, " general", array_message);
    if (!narrow) {
        return;
    }
    walk = kt_iterable_iterator(iterable);
    for (kt_int at = 0; at < elements; at++) {
        (void)kt_range_iterator_next(walk);
    }
    (void)kt_range_iterator_next(walk);
    say_exhausted(label, " narrow", array_message);
}

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef references = kt_array_new(&kt_type_array, 1);
    ((KRef *)((KArray *)references + 1))[0] = kt_box_int(5);
    exhaust("arrayOf(5)", references, 1, 0, "1");
    exhaust("IntArray(1)", kt_array_new(&kt_type_int_array, 1), 1, 1, "1");
    exhaust("CharArray(2)", kt_array_new(&kt_type_char_array, 2), 2, 1, "2");

    exhaust("\"a\"", kt_string_utf8("a", 1), 1, 1, NULL);
    exhaust("\"\"", kt_string_utf8("", 0), 0, 1, NULL);
    exhaust("StringBuilder(\"a\")", kt_string_builder_with_text(kt_string_utf8("a", 1)), 1, 1,
            NULL);

    exhaust("1..1", kt_int_range(1, 1), 1, 1, NULL);
    exhaust("1L..1L", kt_long_range(1, 1), 1, 1, NULL);
    exhaust("'a'..'a'", kt_char_range('a', 'a'), 1, 1, NULL);
    exhaust("1u..1u", kt_uint_range(1, 1), 1, 1, NULL);
    exhaust("2 downTo 1", kt_int_range_down_to(2, 1), 2, 1, NULL);

    KRef one = kt_array_new(&kt_type_array, 1);
    ((KRef *)((KArray *)one + 1))[0] = kt_box_int(1);
    exhaust("listOf(1)", kt_list_of(one), 1, 0, NULL);
    exhaust("listOf()", kt_list_empty(), 0, 0, NULL);
    exhaust("mutableListOf(1)", kt_mutable_list_of(one), 1, 0, NULL);
    exhaust("listOf(1).withIndex()", kt_iterable_with_index(kt_list_of(one)), 1, 0, NULL);
    kt_sys_write(1, "OK\n", 3);
}
