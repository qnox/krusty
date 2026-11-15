/* `removeAll` drops every element the argument contains and answers whether the list changed.
   `retainAll` keeps only those. Both ask the live argument, so removing the list from itself
   empties it, and retaining it leaves it as it was. */
#include "driver_exceptions.h"

static KRef boxed(kt_int value) { return kt_box_int(value); }

static KRef list_of(kt_int a, kt_int b, kt_int c) {
    KRef list = kt_mutable_list_new();
    kt_mutable_list_add(list, boxed(a));
    kt_mutable_list_add(list, boxed(b));
    kt_mutable_list_add(list, boxed(c));
    return list;
}

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    KRef list = list_of(1, 2, 2);
    KRef drop = kt_mutable_list_new();
    kt_mutable_list_add(drop, boxed(2));
    CHECK(kt_mutable_list_remove_all(list, drop), "removeAll of a present element changed nothing\n");
    CHECK(kt_pending_exception() == NULL, "removeAll raised\n");
    CHECK(kt_list_size(list) == 1 && kt_unbox_int(kt_list_get(list, 0)) == 1,
          "removeAll left an element it was given\n");
    CHECK(!kt_mutable_list_remove_all(list, drop), "removeAll of an absent element reported a change\n");

    KRef kept = list_of(1, 2, 3);
    KRef hold = kt_mutable_list_new();
    kt_mutable_list_add(hold, boxed(2));
    CHECK(kt_mutable_list_retain_all(kept, hold), "retainAll of a narrower argument changed nothing\n");
    CHECK(kt_list_size(kept) == 1 && kt_unbox_int(kt_list_get(kept, 0)) == 2,
          "retainAll kept an element the argument lacks\n");
    CHECK(!kt_mutable_list_retain_all(kept, hold), "retainAll of the same elements reported a change\n");

    KRef self = list_of(1, 2, 3);
    CHECK(kt_mutable_list_remove_all(self, self), "removeAll of the list itself changed nothing\n");
    CHECK(kt_list_size(self) == 0, "removeAll of the list itself left an element\n");
    KRef same = list_of(4, 5, 6);
    CHECK(!kt_mutable_list_retain_all(same, same), "retainAll of the list itself reported a change\n");
    CHECK(kt_list_size(same) == 3, "retainAll of the list itself dropped an element\n");
    kt_sys_write(1, "OK\n", 3);
}
