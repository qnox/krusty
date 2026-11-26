/* `listIterator(index)` on a read-only list: the cursor starts at `index`, `previous()` steps back
   and `next()` forward, stepping before the start raises `NoSuchElementException`, and an index
   outside `0..size` raises `IndexOutOfBoundsException`.

   The driver prints what each call answered, and the harness compares the lines with what
   `list_list_iterator.kt` answers under the reference kotlinc. */
#include "driver_exceptions.h"
#include "transcript.h"

static kt_boolean raised(void) {
    kt_boolean thrown = kt_pending_exception() != NULL;
    kt_clear_pending();
    return thrown;
}

static void say_flag(kt_boolean value) {
    say(value ? "true" : "false");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef two = kt_array_new(&kt_type_array, 2);
    ((KRef *)((KArray *)two + 1))[0] = kt_box_int(1);
    ((KRef *)((KArray *)two + 1))[1] = kt_box_int(2);
    KRef list = kt_list_of(two);
    KRef iterator = kt_list_list_iterator(list, 2);
    CHECK(kt_pending_exception() == NULL, "listIterator(2) raised\n");
    say("listOf(1, 2).listIterator(2): ");
    say_flag(kt_list_iterator_has_previous(iterator));
    say(" ");
    say_long(kt_list_iterator_next_index(iterator));
    say(" ");
    say_long(kt_list_iterator_previous_index(iterator));
    say("\nprevious ");
    say_value(kt_list_iterator_previous(iterator));
    say(" ");
    say_value(kt_list_iterator_previous(iterator));
    say(" ");
    say_flag(kt_list_iterator_has_previous(iterator));
    say("\nprevious at the start threw ");
    (void)kt_list_iterator_previous(iterator);
    say_flag(raised());
    say("\nnext ");
    say_value(kt_iterator_next(iterator));
    say(" ");
    say_value(kt_iterator_next(iterator));
    say(" ");
    say_flag(kt_iterator_has_next(iterator));
    say("\nlistIterator(-1) threw ");
    (void)kt_list_list_iterator(list, -1);
    say_flag(raised());
    say("\nlistIterator(3) threw ");
    (void)kt_list_list_iterator(list, 3);
    say_flag(raised());
    say("\nenum entries listIterator is a ListIterator ");
    KRef entries = kt_enum_entries_of(two);
    KRef entry_iterator = kt_list_list_iterator(entries, 1);
    CHECK(kt_pending_exception() == NULL, "entries.listIterator(1) raised\n");
    say_flag(kt_is_instance(entry_iterator, &kt_type_list_iterator_interface));
    say("\n");
    kt_sys_write(1, "OK\n", 3);
}
