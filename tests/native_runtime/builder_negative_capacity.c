/* `StringBuilder(capacity)` with a NEGATIVE capacity throws and makes no builder; zero and a
   positive capacity make an empty builder that grows past what it was given. The runtime used to
   read every negative capacity as zero and hand back an empty builder, so the exception a program
   catches there never came.

   Kotlin's common `StringBuilder(capacity: Int)` documents no exception, and the platforms differ.
   The runtime throws Kotlin/Native's TYPE, `IllegalArgumentException`: Kotlin/Native 2.4.10's
   stdlib (the linux_x64 static cache of the distribution) compiles the constructor to
   `AllocArrayInstance(CharArray, capacity)` with no check of its own, and `AllocArrayInstance` calls
   `ThrowIllegalArgumentException` for a negative size. Its MESSAGE is the JVM's, the capacity:
   Kotlin/JVM throws Java's `NegativeArraySizeException("-1")`. The driver prints what each
   construction made or threw, and the harness compares the lines with what
   `builder_negative_capacity.kt` answers under the reference kotlinc, the exception type on each
   negative capacity's line being a declared divergence. */
#include "transcript.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    static const kt_int capacities[] = {-1, -42, (kt_int)0x80000000u};
    for (unsigned at = 0; at < sizeof(capacities) / sizeof(capacities[0]); at++) {
        KRef made = kt_string_builder_with_capacity(capacities[at]);
        KRef thrown = kt_pending_exception();
        kt_clear_pending();
        CHECK(thrown != NULL && made == NULL, "a negative capacity made a builder\n");
        say("StringBuilder(");
        say_long(capacities[at]);
        say("): ");
        say_thrown(thrown);
        say("\n");
    }

    KRef empty = kt_string_builder_with_capacity(0);
    CHECK(kt_pending_exception() == NULL, "a zero capacity threw\n");
    say("StringBuilder(0): made \"");
    say_text(empty);
    say("\"\n");
    KRef builder = kt_string_builder_with_capacity(4);
    CHECK(kt_pending_exception() == NULL, "a positive capacity threw\n");
    (void)kt_string_builder_append(builder, kt_string_utf8("past the capacity", 17));
    say("StringBuilder(4) grown: \"");
    say_text(builder);
    say("\"\n");
    kt_sys_write(1, "OK\n", 3);
}
