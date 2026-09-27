/* `next()` on an exhausted iterator raises what Kotlin raises there, catchably, through both the
   general protocol (`kt_iterator_next`, a boxed element) and the narrow one
   (`kt_range_iterator_next`, the element's bits):

   - an array's, Kotlin/Native's `ArrayIterator` and `IntArrayIterator` family:
     `NoSuchElementException` whose message is the index asked for (`"1"`);
   - a range's or progression's, a list's and `withIndex()`'s: `NoSuchElementException()`;
   - a `String`'s or a `StringBuilder`'s: whatever reading the text past its end raises, because
     Kotlin's `CharSequence.iterator()` answers `get(index++)` with no check of its own — this
     runtime's `ArrayIndexOutOfBoundsException("Index 1 out of bounds for length 1")` for a
     `String`, Kotlin/Native's type, and `IndexOutOfBoundsException` for a `StringBuilder`.

   The general protocol used to answer a string's exhaustion with `NoSuchElementException`, and
   the narrow one ended the program for an exhausted array, string or range.

   kotlinc 2.4.10 on the JVM agrees on every type but the texts' and on the arrays' message, for
   this program:

       fun t(label: String, f: () -> Any?) = println("$label " + try { f().toString() }
           catch (e: Throwable) { "threw ${e::class.qualifiedName}: ${e.message}" })
       fun main() {
           t("array") { val i = arrayOf(1).iterator(); i.next(); i.next() }
           t("intArray") { val i = intArrayOf(1).iterator(); i.next(); i.nextInt() }
           t("string") { val i = "a".iterator(); i.next(); i.nextChar() }
           t("stringEmpty") { "".iterator().nextChar() }
           t("builder") { val i = StringBuilder("a").iterator(); i.next(); i.nextChar() }
           t("intRange") { val i = (1..1).iterator(); i.next(); i.nextInt() }
           t("listOf") { val i = listOf(1).iterator(); i.next(); i.next() }
           t("withIndex") { val i = listOf(1).withIndex().iterator(); i.next(); i.next() }
       }

   which prints `java.util.NoSuchElementException: Index 1 out of bounds for length 1` for the two
   arrays, `java.lang.StringIndexOutOfBoundsException: Index 1 out of bounds for length 1` (and
   `Index 0 ... length 0`) for the texts, and `java.util.NoSuchElementException: null` for the
   rest.
   The arrays' message is platform-defined: Kotlin/Native 2.4.10's `IntArrayIterator.nextInt` and
   `ArrayIterator.next` (the distribution's linux_x64 stdlib cache) build it as `index.toString()`,
   and this runtime is the native one. */
#include "collections_later_tiers.h"

/* The exception pending is `type` with exactly this message (NULL for none); the slot is
   cleared. */
static kt_boolean raised(const KType *type, const char *message) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    if (thrown == NULL || type_of(thrown) != type) {
        return 0;
    }
    KRef text = kt_throwable_message(thrown);
    if (message == NULL) {
        return text == NULL;
    }
    kt_int length = 0;
    while (message[length] != 0) {
        length++;
    }
    return text != NULL && text_is(text, message, length);
}

static const char past_one[] = "Index 1 out of bounds for length 1";
static const char past_none[] = "Index 0 out of bounds for length 0";

/* Walk `iterable` past its `elements` elements through the general protocol, then again through
   the narrow one when `narrow` says it has one, expecting `type` and `message` each time. */
static void exhaust(KRef iterable, kt_int elements, kt_boolean narrow, const KType *type,
                    const char *message) {
    KRef walk = kt_iterable_iterator(iterable);
    for (kt_int at = 0; at < elements; at++) {
        (void)kt_iterator_next(walk);
    }
    CHECK(kt_pending_exception() == NULL, "an element before the end raised\n");
    CHECK(!kt_iterator_has_next(walk), "an exhausted iterator has a next element\n");
    (void)kt_iterator_next(walk);
    CHECK(raised(type, message), "the general next() of an exhausted iterator\n");
    if (!narrow) {
        return;
    }
    walk = kt_iterable_iterator(iterable);
    for (kt_int at = 0; at < elements; at++) {
        (void)kt_range_iterator_next(walk);
    }
    (void)kt_range_iterator_next(walk);
    CHECK(raised(type, message), "the narrow next() of an exhausted iterator\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    const KType *nsee = &kt_type_no_such_element_exception;
    const KType *ioobe = &kt_type_index_out_of_bounds_exception;
    const KType *aioobe = &kt_type_array_index_out_of_bounds_exception;

    KRef references = kt_array_new(&kt_type_array, 1);
    ((KRef *)((KArray *)references + 1))[0] = kt_box_int(5);
    exhaust(references, 1, 0, nsee, "1");
    exhaust(kt_array_new(&kt_type_int_array, 1), 1, 1, nsee, "1");
    exhaust(kt_array_new(&kt_type_char_array, 2), 2, 1, nsee, "2");

    exhaust(kt_string_utf8("a", 1), 1, 1, aioobe, past_one);
    exhaust(kt_string_utf8("", 0), 0, 1, aioobe, past_none);
    exhaust(kt_string_builder_with_text(kt_string_utf8("a", 1)), 1, 1, ioobe, past_one);

    exhaust(kt_int_range(1, 1), 1, 1, nsee, NULL);
    exhaust(kt_long_range(1, 1), 1, 1, nsee, NULL);
    exhaust(kt_char_range('a', 'a'), 1, 1, nsee, NULL);
    exhaust(kt_uint_range(1, 1), 1, 1, nsee, NULL);
    exhaust(kt_int_range_down_to(2, 1), 2, 1, nsee, NULL);

    KRef one = kt_array_new(&kt_type_array, 1);
    ((KRef *)((KArray *)one + 1))[0] = kt_box_int(1);
    exhaust(kt_list_of(one), 1, 0, nsee, NULL);
    exhaust(kt_list_empty(), 0, 0, nsee, NULL);
    exhaust(kt_mutable_list_of(one), 1, 0, nsee, NULL);
    exhaust(kt_iterable_with_index(kt_list_of(one)), 1, 0, nsee, NULL);
    kt_sys_write(1, "OK\n", 3);
}
