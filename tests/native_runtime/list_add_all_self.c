/* `xs.addAll(xs)` and `xs += xs` append what the list held when the call began: Kotlin's `addAll`
   of a collection copies the argument out first, so `[1, 2]` becomes `[1, 2, 1, 2]`. The runtime
   used to walk the argument with an iterator while appending to it, so the second step saw its own
   list modified, recorded a `ConcurrentModificationException`, appended the NULL `next` came back
   with, and never advanced -- the walk ran forever, growing the list until the heap gave out. */
#include "collections_later_tiers.h"

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    KRef one = kt_box_int(1000);
    KRef two = kt_box_int(2000);
    KRef list = kt_mutable_list_new();
    kt_mutable_list_add(list, one);
    kt_mutable_list_add(list, two);
    kt_mutable_list_add_all(list, list);
    CHECK(kt_pending_exception() == NULL, "addAll of the list itself raised\n");
    CHECK(kt_list_size(list) == 4, "addAll of the list itself did not double it\n");
    CHECK(kt_list_get(list, 0) == one && kt_list_get(list, 1) == two
              && kt_list_get(list, 2) == one && kt_list_get(list, 3) == two,
          "addAll of the list itself appended something other than its elements\n");

    /* An immutable list appended to a growable one, which is the ordinary case, still works. */
    KRef tail = kt_list_single(one);
    kt_mutable_list_add_all(list, tail);
    CHECK(kt_list_size(list) == 5 && kt_list_get(list, 4) == one, "addAll of a list lost it\n");
    kt_sys_write(1, "OK\n", 3);
}
