/* A member access on `null` is Kotlin's `NullPointerException`, with no message, which a program
   may catch: the entry raises it, answers a placeholder nobody reads, and does nothing else. It
   used to stop the program with `krusty: member access on a null receiver`. `kt_dispatch` raises
   too, and answers NULL without reading a descriptor through the null; `dispatch_on_null.c` makes
   the call sequence a call site emits around it.

   The driver prints what `kt_null_receiver` raises, and the harness compares the line with what
   `null_receiver_raises.kt` answers under the reference kotlinc: a null where a receiver belongs,
   reached through `!!` since a Kotlin program has no other way to hold one, raises a
   `NullPointerException` with no message that is a `RuntimeException`. The driver then checks
   every entry that meets a null receiver. On the JVM a member access on a platform type's null raises the
   same class with a message describing the Java call that failed, which a native program has no
   counterpart of. */
#include "driver_exceptions.h"
#include "transcript.h"

static int invoked;

static KRef count_call(KRef self, KRef argument) {
    (void)self;
    invoked++;
    return argument;
}

FUNCTION_VALUE(f_count, count_call)

/* Whether exactly a `NullPointerException` with no message is in flight; takes it. */
static kt_boolean raised_npe(void) {
    return took_message(&kt_type_null_pointer_exception, NULL);
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_null_receiver();
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    CHECK(thrown != NULL && type_of(thrown) == &kt_type_null_pointer_exception,
          "kt_null_receiver did not raise NullPointerException\n");
    /* `"${e::class.simpleName}: ${e.message} ${e is RuntimeException}"`. */
    say_throwable(thrown);
    say(" ");
    say_value(kt_box_boolean(kt_is_instance(thrown, &kt_type_runtime_exception)));
    say("\n");


    CHECK(kt_dispatch(NULL, 0) == NULL && raised_npe(),
          "dispatch on null did not raise NullPointerException and answer NULL\n");

    CHECK(kt_compare_any(NULL, kt_box_int(1)) == 0 && raised_npe(), "compareTo on null\n");
    CHECK(kt_compare_any(kt_box_int(1), NULL) == 0 && raised_npe(), "compareTo(null)\n");
    CHECK(!kt_array_is_empty(NULL) && raised_npe(), "isEmpty() of a null array\n");
    CHECK(kt_array_reversed_array(NULL) == NULL && raised_npe(), "reversedArray() of null\n");
    CHECK(kt_indexed_value_index(NULL) == 0 && raised_npe(), "IndexedValue.index of null\n");
    CHECK(kt_indexed_value_value(NULL) == NULL && raised_npe(), "IndexedValue.value of null\n");
    CHECK(kt_iterator_next(NULL) == NULL && raised_npe(), "next() of a null iterator\n");
    CHECK(kt_iterable_map(NULL, f_count) == NULL && raised_npe() && invoked == 0,
          "map over null went on after raising\n");
    CHECK(kt_class_of(NULL) == NULL && raised_npe(), "::class of null\n");
    CHECK(kt_rw_property_get(NULL, kt_string_utf8("p", 1)) == NULL && raised_npe(),
          "a read through a null delegate\n");
    kt_rw_property_set(NULL, NULL, kt_box_int(1));
    CHECK(raised_npe(), "a write through a null delegate\n");

    CHECK(kt_iterable_iterator(NULL) == NULL && raised_npe(), "iterator() of null\n");
    CHECK(!kt_iterator_has_next(NULL) && raised_npe(), "hasNext() of a null iterator\n");
    CHECK(kt_range_iterator_next(NULL) == 0 && raised_npe(), "nextInt() of a null iterator\n");
    CHECK(kt_array_to_list(NULL) == NULL && raised_npe(), "toList() of a null array\n");
    KRef spread = kt_array_new(&kt_type_int_array, 2);
    CHECK(kt_array_copy_into(spread, 1, NULL) == 1 && raised_npe(), "a spread of null\n");

    kt_null_receiver();
    CHECK(raised_npe(), "kt_null_receiver did not raise NullPointerException\n");
    kt_sys_write(1, "OK\n", 3);
}
