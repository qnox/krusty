/* `ArrayList(-1)` raises `IllegalArgumentException`, as Kotlin's does: a negative capacity is not
   a hint the list may ignore. It used to answer an empty list. A capacity of zero or more is still
   an empty list that takes elements. */
#include "collections_later_tiers.h"

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    CHECK(kt_mutable_list_with_capacity(-1) == NULL, "ArrayList(-1) answered a list\n");
    CHECK(took(&kt_type_illegal_argument_exception), "ArrayList(-1) raised no IAE\n");
    KRef list = kt_mutable_list_with_capacity(0);
    CHECK(list != NULL && kt_list_size(list) == 0, "ArrayList(0) is not an empty list\n");
    kt_mutable_list_add(list, kt_box_int(1));
    CHECK(kt_list_size(list) == 1, "ArrayList(0) did not take an element\n");
    CHECK(kt_pending_exception() == NULL, "ArrayList(0) raised\n");
    kt_sys_write(1, "OK\n", 3);
}
