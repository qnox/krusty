/* Every registered global slot is a root, however many the program registers. The emitted code
   registers one per string literal, enum entry, reference-typed top-level property and `object`,
   so a large program has thousands; the registry used to hold 4096 and end the program on the
   next. */
#include "krusty_rt.h"
#include "krusty_sys.h"

typedef struct Box {
    KObjectHeader header;
    uintptr_t value;
} Box;

static const KType box_type = {.name = "Box", .name_length = 3, .instance_size = sizeof(Box)};

#define ROOTS 10000u

static Box *slots[ROOTS];

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    for (unsigned i = 0; i < ROOTS; i++) {
        kt_gc_add_global_root((void **)&slots[i]);
        slots[i] = (Box *)kt_gc_allocate(&box_type, sizeof(Box));
        slots[i]->value = i;
    }
    kt_gc_collect();
    if (kt_gc_live_objects() != ROOTS) {
        KT_SYS_FAIL("an object a global root holds was freed\n");
    }
    for (unsigned i = 0; i < ROOTS; i++) {
        if (slots[i]->header.type != &box_type || slots[i]->value != i) {
            KT_SYS_FAIL("an object a global root holds was overwritten\n");
        }
    }
    kt_sys_write(1, "OK\n", 3);
}
