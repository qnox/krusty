/* Reading a list outside it raises and answers nothing: `get` raises `IndexOutOfBoundsException`,
   and `first()`/`last()` of an empty list raise `NoSuchElementException`, as Kotlin's do. Each used
   to raise and then read anyway, because `kt_throw` records the exception and comes back: `get(-1)`
   answered the backing array's length word as if it were an element, and `last()` of a growable
   list emptied by `removeAt` answered its capacity the same way. */
#include "collections_later_tiers.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef single = kt_list_single(kt_box_int(7));
    (void)kt_list_get(single, -1);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index -1 out of bounds for length 1"),
          "get(-1) raised no IOOBE\n");
    (void)kt_list_get(single, 1);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index 1 out of bounds for length 1"),
          "get(size) raised no IOOBE\n");
    CHECK(kt_unbox_int(kt_list_get(single, 0)) == 7, "get(0) lost the element\n");
    CHECK(kt_pending_exception() == NULL, "get(0) raised\n");

    /* Emptied by `removeAt`, so its storage still has room for four: the slot before the first
       element is the storage's length, which is what a read of `elements[size - 1]` finds. */
    KRef emptied = kt_mutable_list_new();
    kt_mutable_list_add(emptied, kt_box_int(7));
    (void)kt_mutable_list_remove_at(emptied, 0);
    (void)kt_list_last(emptied);
    CHECK(took_message(&kt_type_no_such_element_exception, "List is empty."),
          "last() of an emptied list raised no NSEE\n");
    (void)kt_list_first(emptied);
    CHECK(took_message(&kt_type_no_such_element_exception, "List is empty."),
          "first() of an emptied list raised no NSEE\n");

    KRef empty = kt_list_empty();
    (void)kt_list_first(empty);
    CHECK(took_message(&kt_type_no_such_element_exception, "List is empty."),
          "first() of an empty list raised no NSEE\n");
    (void)kt_list_last(empty);
    CHECK(took_message(&kt_type_no_such_element_exception, "List is empty."),
          "last() of an empty list raised no NSEE\n");
    kt_sys_write(1, "OK\n", 3);
}
