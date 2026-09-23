/* A `kotlin.Number` conversion on `null` -- `(n as Number?)!!` elided, or a platform null reaching
   `toInt()` -- raises Kotlin's `NullPointerException` and comes back with the exception pending.
   It used to raise and then read the descriptor of the null it had just rejected, so every
   `toByte`/`toShort`/`toInt`/`toLong`/`toFloat`/`toDouble` on null crashed instead. */
#include "later_tiers.h"

static void expect_null_pointer_exception(void) {
    KRef thrown = kt_pending_exception();
    CHECK(thrown != NULL, "a conversion of null left no exception pending\n");
    CHECK(type_of(thrown) == &kt_type_null_pointer_exception,
          "a conversion of null raised something other than a NullPointerException\n");
    kt_clear_pending();
}

void kt_program_entry(void) {
    CHECK(kt_number_to_byte(NULL) == 0, "toByte(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_number_to_short(NULL) == 0, "toShort(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_number_to_int(NULL) == 0, "toInt(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_number_to_long(NULL) == 0, "toLong(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_number_to_float(NULL) == 0.0f, "toFloat(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_number_to_double(NULL) == 0.0, "toDouble(null) answered a value\n");
    expect_null_pointer_exception();

    CHECK(kt_number_to_int(kt_box_double(2.9)) == 2, "toInt of a boxed Double did not truncate\n");
    CHECK(kt_pending_exception() == NULL, "converting a present number raised\n");
    kt_sys_write(1, "OK\n", 3);
}
