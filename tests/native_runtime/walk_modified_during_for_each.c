/* A list modified while `forEach` walks it raises `ConcurrentModificationException` at the next
   step, and the walk ENDS there, as Kotlin's does. The walk used to go on: the iterator recorded the
   exception and came back with NULL, and `forEach` handed that NULL to the action as if it were an
   element -- which here removed another element for it -- until the bound ran out. */
#include "collections_later_tiers.h"

static KRef list;
static int invocations;

static KRef remove_first_invoke(KRef self, KRef element) {
    (void)self;
    (void)element;
    invocations++;
    (void)kt_mutable_list_remove_at(list, 0);
    return NULL;
}

FUNCTION_VALUE(remove_first, remove_first_invoke)

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    list = kt_mutable_list_new();
    kt_gc_add_global_root((void **)&list);
    kt_mutable_list_add(list, kt_box_int(1));
    kt_mutable_list_add(list, kt_box_int(2));
    kt_mutable_list_add(list, kt_box_int(3));
    kt_iterable_for_each(list, remove_first);
    CHECK(took(&kt_type_concurrent_modification_exception),
          "a list modified during forEach raised no CME\n");
    CHECK(invocations == 1, "forEach went on after the list was modified\n");
    CHECK(kt_list_size(list) == 2, "forEach removed more than the action asked\n");
    kt_sys_write(1, "OK\n", 3);
}
