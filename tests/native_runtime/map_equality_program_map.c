/* A map compared with a class of the PROGRAM that implements `Map` fails loudly, naming the class.
   Kotlin/Native's `HashMap.equals` would go on to ask that map its `size` and `entries`, which are
   the program's own members, and this runtime has no way to call them: answering `false` would be
   a guess that a map of the same entries contradicts. Before, a map was equal only to one of its
   own class, so such a comparison quietly answered false. */
#include "collections_later_tiers.h"

static const KType *const program_map_interfaces[] = {&kt_type_map_interface};

static const KType program_map_type = {
    .name = "ProgramMap",
    .name_length = sizeof("ProgramMap") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .interfaces = program_map_interfaces,
    .interface_count = 1,
};

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);

    KRef program_map = kt_gc_allocate(&program_map_type, sizeof(KObjectHeader));
    (void)kt_equals(kt_map_new(), program_map);
    kt_sys_write(1, "OK\n", 3);
}
