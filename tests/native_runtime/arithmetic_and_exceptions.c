/* Kotlin's integer arithmetic where C's differs or is undefined, and the exceptions and wording a
   failing operation or assertion reports.

   The driver prints one line per claim, and the harness compares the lines with what
   `arithmetic_and_exceptions.kt` answers under the reference kotlinc, with kotlin-test on its
   class path. A message built from a class NAME is Kotlin/Native's, `kotlin.IllegalStateException`
   where the JVM names `java.lang.IllegalStateException` (`Throwable(cause)`'s message and
   `assertFailsWith`'s report), otherwise the JVM's character for character; those lines are
   declared divergences. */
#include "transcript.h"

#define INT_MIN_VALUE ((kt_int)0x80000000u)
#define LONG_MIN_VALUE ((kt_long)0x8000000000000000ull)

static KRef int_(kt_int value) { return kt_box_int(value); }
static KRef long_(kt_long value) { return kt_box_long(value); }
static KRef boolean_(kt_boolean value) { return kt_box_boolean(value); }

__attribute__((noinline)) static void integers(void) {
    say_line("Int.MIN_VALUE / -1", int_(kt_div_int(INT_MIN_VALUE, -1)));
    say_line("Long.MIN_VALUE / -1", long_(kt_div_long(LONG_MIN_VALUE, -1)));
    say_line("-7 % 2", int_(kt_rem_int(-7, 2)));
    say_line("7 % -2", int_(kt_rem_int(7, -2)));
    say_line("Int.MIN_VALUE % -1", int_(kt_rem_int(INT_MIN_VALUE, -1)));
    say_line("(-7).mod(2)", int_(kt_mod_int(-7, 2)));
    say_line("7.mod(-2)", int_(kt_mod_int(7, -2)));
    say_line("Int.MIN_VALUE.mod(3)", int_(kt_mod_int(INT_MIN_VALUE, 3)));
    say_line("5.mod(Int.MIN_VALUE)", int_(kt_mod_int(5, INT_MIN_VALUE)));
    say_line("Long.MIN_VALUE.mod(3L)", long_(kt_mod_long(LONG_MIN_VALUE, 3)));
    say_line("1 shl 32", int_(kt_shl_int(1, 32)));
    say_line("1 shl -1", int_(kt_shl_int(1, -1)));
    say_line("Int.MIN_VALUE shr 32", int_(kt_shr_int(INT_MIN_VALUE, 32)));
    say_line("-8 shr 1", int_(kt_shr_int(-8, 1)));
    say_line("-1 ushr 28", int_(kt_ushr_int(-1, 28)));
    say_line("-1L ushr 64", long_(kt_ushr_long(-1, 64)));
    say_line("Long.MIN_VALUE shr 63", long_(kt_shr_long(LONG_MIN_VALUE, 63)));
    say_line("abs(Int.MIN_VALUE)", int_(kt_abs_int(INT_MIN_VALUE)));
    say_line("abs(-5L)", long_(kt_abs_long(-5)));
    say_line("abs(-0.0).toRawBits()", long_(kt_double_to_raw_bits(kt_abs_double(-0.0))));
}

__attribute__((noinline)) static void unsigned_integers(void) {
    say_line("ULong.MAX_VALUE / 2uL", kt_box_ulong(kt_div_ulong(-1, 2)));
    say_line("(1uL shl 63) % 3uL", kt_box_ulong(kt_rem_ulong(LONG_MIN_VALUE, 3)));
    say_line("(UInt.MAX_VALUE - 1u) / 2u", kt_box_uint(kt_div_uint(-2, 2)));
    say_line("ULong.MAX_VALUE", kt_ulong_to_string(-1));
    say_line("(-128).toUByte()", kt_ubyte_to_string(-128));
    say_line("UInt.MAX_VALUE", kt_uint_to_string(-1));
}

__attribute__((noinline)) static void floating_order(void) {
    kt_double nan = kt_double_from_bits(0x7FF8000000000000ll);
    kt_double infinity = kt_double_from_bits(0x7FF0000000000000ll);
    say_line("(-0.0).compareTo(0.0)", int_(kt_compare_double(-0.0, 0.0)));
    say_line("NaN.compareTo(POSITIVE_INFINITY)", int_(kt_compare_double(nan, infinity)));
    say_line("NaN.compareTo(NaN)", int_(kt_compare_double(nan, nan)));
    say_line("(-0.0f).compareTo(0.0f)", int_(kt_compare_float((kt_float)-0.0, (kt_float)0.0)));
}

__attribute__((noinline)) static void exceptions(void) {
    CHECK(kt_div_int(1, 0) == 0, "1 / 0 answered something\n");
    KRef thrown = kt_pending_exception();
    say_raised("1 / 0");
    say_line("1 / 0 is ArithmeticException",
             boolean_(kt_is_instance(thrown, &kt_type_arithmetic_exception)));
    say_line("1 / 0 is RuntimeException",
             boolean_(kt_is_instance(thrown, &kt_type_runtime_exception)));
    say_line("1 / 0 is Error", boolean_(kt_is_instance(thrown, &kt_type_error)));
    (void)kt_rem_ulong(1, 0);
    say_raised("1uL % 0uL");

    KRef bare = kt_throwable_new(&kt_type_illegal_state_exception, NULL);
    say("IllegalStateException() = ");
    say_throwable(bare);
    say("\n");
    KRef wrapped = kt_throwable_new_from_cause(&kt_type_runtime_exception, bare);
    say_line("RuntimeException(bare).cause === bare",
             boolean_(kt_throwable_cause(wrapped) == bare));
    say_line("RuntimeException(bare).message", kt_throwable_message(wrapped));
}

__attribute__((noinline)) static void assertions(void) {
    kt_assert_equals(kt_box_int(1), kt_box_int(2), NULL);
    say_raised("assertEquals(1, 2)");
    kt_assert_true(false, kt_string_utf8("m", 1));
    say_raised("assertTrue(false, \"m\")");
    kt_assert_equals(kt_box_int(1), kt_box_int(1), NULL);
    CHECK(kt_pending_exception() == NULL, "a passing assertion raised\n");
    say_line("assertEquals(1, 1)", kt_unit());

    kt_assert_failed_to_throw(NULL, &kt_type_illegal_state_exception, NULL);
    say_raised("assertFailsWith<IllegalStateException> completing");
    KRef was = kt_throwable_new(&kt_type_arithmetic_exception, kt_string_utf8("/ by zero", 9));
    kt_assert_failed_to_throw(kt_string_utf8("m", 1), &kt_type_illegal_state_exception, was);
    say_raised("assertFailsWith<IllegalStateException>(\"m\") throwing");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    integers();
    unsigned_integers();
    floating_order();
    exceptions();
    assertions();
    /* Nothing is in flight, so the check the generated entry makes before exiting returns. */
    kt_check_uncaught();
    kt_sys_write(1, "OK\n", 3);
}
