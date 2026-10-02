/* `ArrayList(-1)` raises `IllegalArgumentException`, as Kotlin's does on both platforms: a negative
   capacity is not a hint the list may ignore. It used to answer an empty list. A capacity of zero
   or more is still an empty list that takes elements. The message is the JVM's, `Illegal Capacity:
   -1`, where Kotlin/Native's `arrayOfUninitializedElements` says `capacity must be non-negative.`.

   The driver prints what each capacity answered, and the harness compares the lines with what
   `array_list_negative_capacity.kt` answers under the reference kotlinc. */
#include "driver_exceptions.h"
#include "transcript.h"

static void say_capacity(const char *label, kt_int capacity) {
    KRef list = kt_mutable_list_with_capacity(capacity);
    say(label);
    say(": ");
    KRef thrown = kt_pending_exception();
    if (thrown != NULL) {
        kt_clear_pending();
        say_thrown(thrown);
    } else {
        kt_mutable_list_add(list, kt_box_int(1));
        CHECK(kt_pending_exception() == NULL, "adding to an ArrayList raised\n");
        say_value(list);
    }
    say("\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    say_capacity("ArrayList(-1)", -1);
    say_capacity("ArrayList(-2147483648)", -2147483647 - 1);
    say_capacity("ArrayList(0)", 0);
    say_capacity("ArrayList(4)", 4);
    kt_sys_write(1, "OK\n", 3);
}
