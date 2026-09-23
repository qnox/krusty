/* Kotlin's integer arithmetic where C's differs or is undefined, and the exceptions and wording a
   failing operation or assertion reports. Each answer here is what kotlinc's program prints. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#define INT_MIN_VALUE ((kt_int)0x80000000u)
#define LONG_MIN_VALUE ((kt_long)0x8000000000000000ull)
#define LONG_MAX_VALUE ((kt_long)0x7FFFFFFFFFFFFFFFll)

#define CHECK(condition, what)                                                                     \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL(what "\n");                                                                \
        }                                                                                          \
    } while (0)

static kt_boolean text_is(KRef actual, const char *expected, kt_int length) {
    return kt_equals(kt_string_utf8(expected, length), actual);
}

#define TEXT_IS(actual, literal) text_is(actual, literal, (kt_int)(sizeof(literal) - 1))

/* What a `catch (e: Throwable)` would take, and the slot left empty as the clause leaves it. */
static KRef take_pending(void) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    return thrown;
}

__attribute__((noinline)) static void integers(void) {
    CHECK(kt_div_int(INT_MIN_VALUE, -1) == INT_MIN_VALUE, "Int.MIN_VALUE / -1 wraps");
    CHECK(kt_div_long(LONG_MIN_VALUE, -1) == LONG_MIN_VALUE, "Long.MIN_VALUE / -1 wraps");
    CHECK(kt_rem_int(-7, 2) == -1 && kt_rem_int(7, -2) == 1, "% takes the dividend's sign");
    CHECK(kt_rem_int(INT_MIN_VALUE, -1) == 0, "Int.MIN_VALUE % -1 is 0");
    CHECK(kt_mod_int(-7, 2) == 1 && kt_mod_int(7, -2) == -1, "mod takes the divisor's sign");
    CHECK(kt_mod_int(INT_MIN_VALUE, 3) == 1, "Int.MIN_VALUE.mod(3) is 1");
    CHECK(kt_mod_int(5, INT_MIN_VALUE) == 5 + INT_MIN_VALUE, "5.mod(Int.MIN_VALUE) wraps");
    CHECK(kt_mod_long(LONG_MIN_VALUE, 3) == 1, "Long.MIN_VALUE.mod(3) is 1");

    CHECK(kt_shl_int(1, 32) == 1 && kt_shl_int(1, -1) == INT_MIN_VALUE, "shl masks its count");
    CHECK(kt_shr_int(INT_MIN_VALUE, 32) == INT_MIN_VALUE, "shr masks its count");
    CHECK(kt_shr_int(-8, 1) == -4, "shr is arithmetic");
    CHECK(kt_ushr_int(-1, 28) == 15, "ushr fills with zeros");
    CHECK(kt_ushr_long(-1, 64) == -1 && kt_shr_long(LONG_MIN_VALUE, 63) == -1,
          "the Long shifts mask to six bits");

    CHECK(kt_abs_int(INT_MIN_VALUE) == INT_MIN_VALUE, "abs(Int.MIN_VALUE) wraps");
    CHECK(kt_abs_long(-5) == 5, "abs(-5L) is 5");
    CHECK(kt_double_to_raw_bits(kt_abs_double(-0.0)) == 0, "abs(-0.0) is 0.0");
}

__attribute__((noinline)) static void unsigned_integers(void) {
    CHECK(kt_div_ulong(-1, 2) == LONG_MAX_VALUE, "ULong.MAX_VALUE / 2 is Long.MAX_VALUE");
    CHECK(kt_rem_ulong(LONG_MIN_VALUE, 3) == 2, "2^63 % 3 is 2 read unsigned");
    CHECK(kt_div_uint(-2, 2) == 0x7FFFFFFF, "UInt.MAX_VALUE - 1 / 2 reads its operands unsigned");
    CHECK(TEXT_IS(kt_ulong_to_string(-1), "18446744073709551615"),
          "ULong.MAX_VALUE.toString()");
    CHECK(TEXT_IS(kt_ubyte_to_string(-128), "128"), "(-128).toUByte().toString()");
    CHECK(TEXT_IS(kt_uint_to_string(-1), "4294967295"), "UInt.MAX_VALUE.toString()");
}

__attribute__((noinline)) static void floating_order(void) {
    kt_double nan = kt_double_from_bits(0x7FF8000000000000ll);
    kt_double infinity = kt_double_from_bits(0x7FF0000000000000ll);
    CHECK(kt_compare_double(-0.0, 0.0) == -1, "-0.0 orders below 0.0");
    CHECK(kt_compare_double(nan, infinity) == 1, "NaN orders above infinity");
    CHECK(kt_compare_double(nan, nan) == 0, "NaN compares equal to itself");
    CHECK(kt_compare_float((kt_float)-0.0, (kt_float)0.0) == -1, "-0.0f orders below 0.0f");
}

__attribute__((noinline)) static void exceptions(void) {
    CHECK(kt_div_int(1, 0) == 0, "1 / 0 answers without dividing");
    KRef thrown = take_pending();
    CHECK(thrown != NULL, "1 / 0 records an exception");
    CHECK(kt_is_instance(thrown, &kt_type_arithmetic_exception) &&
              kt_is_instance(thrown, &kt_type_runtime_exception),
          "1 / 0 records an ArithmeticException, a RuntimeException");
    CHECK(!kt_is_instance(thrown, &kt_type_error), "an ArithmeticException is not an Error");
    CHECK(TEXT_IS(kt_to_string(thrown), "kotlin.ArithmeticException: / by zero"),
          "an exception renders as its qualified name and message");

    kt_rem_ulong(1, 0);
    CHECK(kt_is_instance(take_pending(), &kt_type_arithmetic_exception),
          "an unsigned % 0 records an ArithmeticException");

    KRef bare = kt_throwable_new(&kt_type_illegal_state_exception, NULL);
    CHECK(TEXT_IS(kt_to_string(bare), "kotlin.IllegalStateException"),
          "an exception without a message renders as its name alone");
    KRef wrapped = kt_throwable_new_from_cause(&kt_type_runtime_exception, bare);
    CHECK(kt_throwable_cause(wrapped) == bare, "Throwable(cause) keeps its cause");
    CHECK(TEXT_IS(kt_throwable_message(wrapped), "kotlin.IllegalStateException"),
          "Throwable(cause) takes its message from the cause");
}

__attribute__((noinline)) static void assertions(void) {
    kt_assert_failed_to_throw(NULL, &kt_type_illegal_state_exception, NULL);
    KRef failure = take_pending();
    CHECK(kt_is_instance(failure, &kt_type_assertion_error), "assertFailsWith raises AssertionError");
    CHECK(TEXT_IS(kt_throwable_message(failure),
                  "Expected an exception of class kotlin.IllegalStateException to be thrown, but "
                  "was completed successfully."),
          "assertFailsWith on a block that completed");

    KRef was = kt_throwable_new(&kt_type_arithmetic_exception, kt_string_utf8("/ by zero", 9));
    kt_assert_failed_to_throw(kt_string_utf8("m", 1), &kt_type_illegal_state_exception, was);
    CHECK(TEXT_IS(kt_throwable_message(take_pending()),
                  "m. Expected an exception of class kotlin.IllegalStateException to be thrown, "
                  "but was kotlin.ArithmeticException: / by zero"),
          "assertFailsWith with a message, on a block that threw something else");

    kt_assert_equals(kt_box_int(1), kt_box_int(2), NULL);
    CHECK(TEXT_IS(kt_throwable_message(take_pending()), "Expected <1>, actual <2>."),
          "assertEquals wording");
    kt_assert_true(false, kt_string_utf8("m", 1));
    CHECK(TEXT_IS(kt_throwable_message(take_pending()), "m. Expected value to be true."),
          "assertTrue wording with a message");
    kt_assert_equals(kt_box_int(1), kt_box_int(1), NULL);
    CHECK(kt_pending_exception() == NULL, "a passing assertion records nothing");
}

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    integers();
    unsigned_integers();
    floating_order();
    exceptions();
    assertions();
    /* Nothing is in flight, so the check the generated entry makes before exiting returns. */
    kt_check_uncaught();
    kt_sys_write(1, "OK\n", 3);
}
