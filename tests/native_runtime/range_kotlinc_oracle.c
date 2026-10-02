/* A stepped range's, a reversed progression's and an unsigned stepped range's bounds and
   membership, and whether a progression and a range of the same elements compare equal each way.
   The questions are 463b8dae's oracle case on origin/rt/06, carried over onto the transcript
   harness: the driver prints each answer, and the harness compares the lines with what
   `range_kotlinc_oracle.kt` answers under the reference kotlinc. */
#include "transcript.h"

static void line(const char *label, kt_boolean answer) {
    say(label);
    say(": ");
    say_bool(answer);
    say("\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef stepped = kt_range_step(kt_int_range(1, 10), 2);
    KRef reversed = kt_range_reversed(kt_range_step(kt_int_range(1, 9), 3));
    KRef progression = kt_range_step(kt_int_range(1, 3), 1);
    KRef range = kt_int_range(1, 3);
    /* `Long.MAX_VALUE.toULong() - 1uL..Long.MAX_VALUE.toULong() + 5uL step 3`: the bounds straddle
       the signed boundary, where an unsigned comparison is not a signed one. */
    KRef unsigned_stepped = kt_range_step(
        kt_ulong_range((kt_long)0x7ffffffffffffffeull, (kt_long)0x8000000000000004ull), 3);

    line("(1..10 step 2).first == 1", kt_range_first(stepped) == 1);
    line("(1..10 step 2).last == 9", kt_range_last(stepped) == 9);
    line("(1..9 step 3).reversed().first == 7", kt_range_first(reversed) == 7);
    line("(1..9 step 3).reversed().last == 1", kt_range_last(reversed) == 1);
    line("(1..3 step 1) == 1..3", kt_equals(progression, range));
    line("1..3 != (1..3 step 1)", !kt_equals(range, progression));
    line("Long.MAX_VALUE.toULong() + 2uL in the unsigned progression",
         kt_range_contains(unsigned_stepped, (kt_long)0x8000000000000001ull));
    CHECK(kt_pending_exception() == NULL, "a range question raised\n");
    kt_sys_write(1, "OK\n", 3);
}
