/* Unboxing `null` raises Kotlin's `NullPointerException` and comes back, the way every raise does:
   the caller polls the pending slot and finds it. The unbox used to raise and then read the field
   of the null it had just rejected, so `(null as Any?) as Int` crashed before any caller could
   catch it.

   Each unbox must come back with the exception pending and a zero, and the pending slot is cleared
   between them so each one is seen to raise its own. */
#include "later_tiers.h"

static void expect_null_pointer_exception(void) {
    KRef thrown = kt_pending_exception();
    CHECK(thrown != NULL, "unboxing null left no exception pending\n");
    CHECK(type_of(thrown) == &kt_type_null_pointer_exception,
          "unboxing null raised something other than a NullPointerException\n");
    kt_clear_pending();
}

void kt_program_entry(void) {
    CHECK(kt_unbox_byte(NULL) == 0, "unbox_byte(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_unbox_short(NULL) == 0, "unbox_short(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_unbox_int(NULL) == 0, "unbox_int(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_unbox_long(NULL) == 0, "unbox_long(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_unbox_char(NULL) == 0, "unbox_char(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_unbox_boolean(NULL) == 0, "unbox_boolean(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_unbox_float(NULL) == 0.0f, "unbox_float(null) answered a value\n");
    expect_null_pointer_exception();
    CHECK(kt_unbox_double(NULL) == 0.0, "unbox_double(null) answered a value\n");
    expect_null_pointer_exception();

    /* A box that is there still unboxes, and raises nothing. */
    CHECK(kt_unbox_int(kt_box_int(1000)) == 1000, "unbox_int lost the boxed value\n");
    CHECK(kt_pending_exception() == NULL, "unboxing a present box raised\n");
    kt_sys_write(1, "OK\n", 3);
}
