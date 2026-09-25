/* A size within 15 bytes of 4 GiB used to round up to a multiple of 16 by wrapping to a tiny one.
   The object it made lay in a chunk whose end was its start, so no root could resolve to it, and
   the next collection unmapped it while it was held. Such a size cannot be allocated: the request
   must end the program as running out of memory does.

   This is what the runtime's `arrayOfNulls<Any>(536870911)` computes: 16 + 536870911 * 8 in 32
   bits. The harness expects the out-of-memory failure; anything this driver prints itself is the
   defect. */
#include "krusty_rt.h"
#include "krusty_sys.h"

static const KType blob_type = {.name = "Blob", .name_length = 4};

static void *held;

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    kt_gc_add_global_root(&held);
    held = kt_gc_allocate(&blob_type, 0xFFFFFFF8u);
    kt_gc_collect();
    if (kt_gc_live_objects() != 1) {
        KT_SYS_FAIL("an object a root holds was freed\n");
    }
    KT_SYS_FAIL("a size past the largest object was allocated\n");
}
