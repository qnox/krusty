/* A write through a growable list at an index outside it raises `IndexOutOfBoundsException` and
   leaves the list as it was, as Kotlin's does. `set`, `add(index, e)` and `removeAt` used to raise
   and then do the write anyway, because `kt_throw` records the exception and comes back: `set(-1)`
   stored over the backing array's length and handed back the length word as the old element,
   `add(5, e)` wrote past the storage, and `removeAt(0)` on an empty list drove its size to -1.

   Each bad call must come back with the exception pending, and afterwards the list must hold what
   it held, and an iterator made before the calls must walk it without a concurrent modification:
   a call that changed nothing is not a modification. */
#include "driver_exceptions.h"

static void expect_unchanged(KRef list, KRef first, KRef second) {
    CHECK(kt_list_size(list) == 2, "a rejected write changed the list's size\n");
    CHECK(kt_list_get(list, 0) == first, "a rejected write changed the first element\n");
    CHECK(kt_list_get(list, 1) == second, "a rejected write changed the second element\n");
    CHECK(kt_pending_exception() == NULL, "reading a list in bounds raised\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef first = kt_box_int(1000);
    KRef second = kt_box_int(2000);
    KRef intruder = kt_box_int(3000);
    KRef list = kt_mutable_list_new();
    kt_mutable_list_add(list, first);
    kt_mutable_list_add(list, second);
    KRef walk = kt_iterable_iterator(list);

    (void)kt_mutable_list_set(list, -1, intruder);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index -1 out of bounds for length 2"),
          "set(-1) raised no IOOBE\n");
    expect_unchanged(list, first, second);
    (void)kt_mutable_list_set(list, 2, intruder);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index 2 out of bounds for length 2"),
          "set(size) raised no IOOBE\n");
    expect_unchanged(list, first, second);

    kt_mutable_list_add_at(list, 5, intruder);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index 5 out of bounds for length 2"),
          "add(5, e) raised no IOOBE\n");
    expect_unchanged(list, first, second);
    kt_mutable_list_add_at(list, -1, intruder);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index -1 out of bounds for length 2"),
          "add(-1, e) raised no IOOBE\n");
    expect_unchanged(list, first, second);

    (void)kt_mutable_list_remove_at(list, 2);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index 2 out of bounds for length 2"),
          "removeAt(size) raised no IOOBE\n");
    expect_unchanged(list, first, second);
    (void)kt_mutable_list_remove_at(list, -1);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index -1 out of bounds for length 2"),
          "removeAt(-1) raised no IOOBE\n");
    expect_unchanged(list, first, second);

    CHECK(kt_iterator_next(walk) == first, "the walk lost the first element\n");
    CHECK(kt_iterator_next(walk) == second, "the walk lost the second element\n");
    CHECK(kt_pending_exception() == NULL, "a rejected write counted as a modification\n");
    CHECK(!kt_iterator_has_next(walk), "the walk found a third element\n");

    KRef empty = kt_mutable_list_new();
    (void)kt_mutable_list_remove_at(empty, 0);
    CHECK(took_message(&kt_type_index_out_of_bounds_exception,
                       "Index 0 out of bounds for length 0"),
          "removeAt(0) of nothing raised no IOOBE\n");
    CHECK(kt_list_size(empty) == 0, "removeAt(0) of nothing changed the size\n");
    kt_sys_write(1, "OK\n", 3);
}
