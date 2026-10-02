/* `kotlin.test`'s failure wording, which a program reads from the `AssertionError` it catches.
   `assertTrue` and `assertFalse` given a message report THAT message alone, where the runtime
   joined its own text after it; `assertNotSame` reports `Expected not same as <x>.`, where the
   runtime used `assertNotEquals`' `Illegal value: <x>.`. The rest pin the wording already right.
   The texts come from kotlin-test's common `DefaultAsserter`, which Kotlin/Native shares.

   The driver prints what each assertion raised, and the harness compares the lines with what
   `assertion_wording.kt` answers under the reference kotlinc, with kotlin-test on its class
   path. */
#include "driver_exceptions.h"
#include "transcript.h"

static KRef text(const char *bytes) {
    kt_int length = 0;
    while (bytes[length] != 0) {
        length++;
    }
    return kt_string_utf8(bytes, length);
}

/* The transcript's line for the assertion just made: `label = SimpleName: message` when it raised,
   which must be an `AssertionError`, and `label = passed` when it did not. */
static void outcome(const char *label) {
    KRef thrown = kt_pending_exception();
    CHECK(thrown == NULL || type_of(thrown) == &kt_type_assertion_error,
          "an assertion raised something other than AssertionError\n");
    if (thrown == NULL) {
        say(label);
        say(" = passed\n");
        return;
    }
    say_raised(label);
}

/* `listOf(1)`, a fresh one each call. */
static KRef list_of_one(void) {
    KRef array = kt_array_new(&kt_type_array, 1);
    ((KRef *)((char *)array + kt_type_array.instance_size))[0] = kt_box_int(1);
    return kt_list_of(array);
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef m = text("m");

    KRef x = list_of_one();
    kt_assert_true(false, NULL);
    outcome("assertTrue(false)");
    kt_assert_true(false, m);
    outcome("assertTrue(false, m)");
    kt_assert_false(true, NULL);
    outcome("assertFalse(true)");
    kt_assert_false(true, m);
    outcome("assertFalse(true, m)");
    kt_assert_equals(kt_box_int(1), kt_box_int(2), m);
    outcome("assertEquals(1, 2, m)");
    kt_assert_same(list_of_one(), list_of_one(), NULL);
    outcome("assertSame([1], [1])");
    kt_assert_same(list_of_one(), list_of_one(), m);
    outcome("assertSame([1], [1], m)");
    kt_assert_not_same(x, x, NULL);
    outcome("assertNotSame(x, x)");
    kt_assert_not_same(x, x, m);
    outcome("assertNotSame(x, x, m)");
    kt_assert_equals(NULL, kt_box_int(2), NULL);
    outcome("assertEquals(null, 2)");
    kt_assert_true(true, m);
    outcome("assertTrue(true, m)");
    kt_assert_false(false, m);
    outcome("assertFalse(false, m)");
    kt_assert_not_same(list_of_one(), list_of_one(), m);
    outcome("assertNotSame([1], [1], m)");
    kt_sys_write(1, "OK\n", 3);
}
