/* A list's modification count wraps past `Int.MAX_VALUE`, as the JVM's `modCount` does, and an
   iterator still notices the change: the count was a signed `Int` incremented past its maximum,
   which is undefined in C, and under the harness's trapping build the add that took it there ended
   the driver with SIGILL. A list is set to the count directly, through the layout it shares with
   this mirror, rather than modified 2^31 times. */
#include "driver_exceptions.h"

typedef struct MirrorList {
    KObjectHeader header;
    KRef elements;
    kt_int size;
    uint32_t modifications;
} MirrorList;

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef list = kt_mutable_list_new();
    kt_mutable_list_add(list, kt_box_int(1));
    ((MirrorList *)list)->modifications = 0x7fffffffu;
    KRef before = kt_list_iterator(list);
    kt_mutable_list_add(list, kt_box_int(2));
    CHECK(((MirrorList *)list)->modifications == 0x80000000u, "the count did not wrap\n");
    (void)kt_iterator_next(before);
    CHECK(took(&kt_type_concurrent_modification_exception),
          "an iterator made before the add did not notice it\n");
    KRef after = kt_list_iterator(list);
    CHECK(kt_unbox_int(kt_iterator_next(after)) == 1 && kt_unbox_int(kt_iterator_next(after)) == 2,
          "an iterator made after the add\n");
    CHECK(kt_pending_exception() == NULL, "walking the list raised\n");
    kt_sys_write(1, "OK\n", 3);
}
