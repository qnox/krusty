/* Reading a list outside it raises and answers nothing: `get` raises `IndexOutOfBoundsException`,
   and `first()`/`last()` of an empty list raise `NoSuchElementException`, as Kotlin's do. Each used
   to raise and then read anyway, because `kt_throw` records the exception and comes back: `get(-1)`
   answered the backing array's length word as if it were an element, and `last()` of a growable
   list emptied by `removeAt` answered its capacity the same way. */
#include "collections_later_tiers.h"

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    KRef single = kt_list_single(kt_box_int(7));
    CHECK(kt_list_get(single, -1) == NULL, "get(-1) answered something\n");
    CHECK(took(&kt_type_index_out_of_bounds_exception), "get(-1) raised no IOOBE\n");
    CHECK(kt_list_get(single, 1) == NULL, "get(size) answered something\n");
    CHECK(took(&kt_type_index_out_of_bounds_exception), "get(size) raised no IOOBE\n");
    CHECK(kt_unbox_int(kt_list_get(single, 0)) == 7, "get(0) lost the element\n");
    CHECK(kt_pending_exception() == NULL, "get(0) raised\n");

    /* Emptied by `removeAt`, so its storage still has room for four: the slot before the first
       element is the storage's length, which is what a read of `elements[size - 1]` finds. */
    KRef emptied = kt_mutable_list_new();
    kt_mutable_list_add(emptied, kt_box_int(7));
    (void)kt_mutable_list_remove_at(emptied, 0);
    CHECK(kt_list_last(emptied) == NULL, "last() of an emptied list answered something\n");
    CHECK(took(&kt_type_no_such_element_exception), "last() of an emptied list raised no NSEE\n");
    CHECK(kt_list_first(emptied) == NULL, "first() of an emptied list answered something\n");
    CHECK(took(&kt_type_no_such_element_exception), "first() of an emptied list raised no NSEE\n");

    KRef empty = kt_list_empty();
    CHECK(kt_list_first(empty) == NULL, "first() of an empty list answered something\n");
    CHECK(took(&kt_type_no_such_element_exception), "first() of an empty list raised no NSEE\n");
    CHECK(kt_list_last(empty) == NULL, "last() of an empty list answered something\n");
    CHECK(took(&kt_type_no_such_element_exception), "last() of an empty list raised no NSEE\n");
    kt_sys_write(1, "OK\n", 3);
}
