/* A spread copy that does not fit its destination raises `IndexOutOfBoundsException` and writes
   NOTHING. `kt_throw` records the exception and comes back, and the copy went on to `memcpy` past
   the destination's end anyway, overwriting whatever the heap held next; and its bound was
   `at + length` in 32-bit arithmetic, which overflows for an `at` near `Int.MAX_VALUE` and then
   passed the check.

   A spread Kotlin compiles never asks for a copy that does not fit, since the generator sizes the
   array from the spread itself, so the refusals below are the runtime's own safety contract and
   have no Kotlin answer to compare with. The copy that fits does: the driver prints the array it
   fills, and the harness compares it with what `array_copy_into_bounds.kt`'s
   `f(0, 0, 0, *intArrayOf(9, 9))` answers under the reference kotlinc. */
#include "transcript.h"

static kt_int *elements(KRef array) { return (kt_int *)((char *)array + sizeof(KArray)); }

static KRef filled(kt_int length, kt_int value) {
    KRef array = kt_array_new(&kt_type_int_array, length);
    for (kt_int index = 0; index < length; index++) {
        elements(array)[index] = value;
    }
    return array;
}

/* The copy must have thrown `IndexOutOfBoundsException`, and left the destination and the object
   the heap placed after it exactly as they were. */
static void expect_refused(KRef destination, KRef neighbour) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    CHECK(thrown != NULL, "a copy that does not fit threw nothing\n");
    CHECK(((const KObjectHeader *)thrown)->type == &kt_type_index_out_of_bounds_exception,
          "a copy that does not fit threw something other than IndexOutOfBoundsException\n");
    for (kt_int index = 0; index < 2; index++) {
        CHECK(elements(destination)[index] == 1, "a refused copy wrote into its destination\n");
    }
    for (kt_int index = 0; index < 64; index++) {
        CHECK(elements(neighbour)[index] == 2, "a refused copy wrote past its destination\n");
    }
}

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef destination = filled(2, 1);
    KRef neighbour = filled(64, 2);
    KRef source = filled(4, 7);

    /* Too long for where it starts: the old code copied three elements past the end. */
    kt_array_copy_into(destination, 1, source);
    expect_refused(destination, neighbour);

    /* A start before the destination. */
    kt_array_copy_into(destination, -1, filled(1, 7));
    expect_refused(destination, neighbour);

    /* A start near `Int.MAX_VALUE`, where `at + length` wraps negative in 32 bits. */
    kt_array_copy_into(destination, INT32_MAX - 1, source);
    expect_refused(destination, neighbour);

    /* A copy that fits lands where it was asked and answers the index after it. */
    KRef two = filled(2, 9);
    KRef wide = filled(5, 0);
    CHECK(kt_array_copy_into(wide, 3, two) == 5, "the copy did not answer the index after it\n");
    CHECK(kt_pending_exception() == NULL, "a copy that fits threw\n");
    say("[");
    for (kt_int index = 0; index < 5; index++) {
        say(index == 0 ? "" : ", ");
        say_long(elements(wide)[index]);
    }
    say("]\n");

    kt_sys_write(1, "OK\n", 3);
}
