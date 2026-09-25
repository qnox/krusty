/* A pointer one past the end of an ARRAY's elements keeps that array alive, and a pointer to a
   plain object's start keeps nothing but that object. A C compiler walking an array may keep only
   its end pointer once the base is dead; when the array fills its slot exactly, that address is
   the next slot's start or past the chunk altogether, and the array used to be freed while the
   loop was still reading it.

   Both shapes are checked: a small array that fills its size class, and a large one whose size is
   a multiple of 16 so it fills its own chunk. The only copies left are the end pointers, in
   volatile locals, and the stack below is scrubbed so a stale base pointer cannot stand in.

   The converse is checked too: a plain object allocated just below a held one is NOT kept by the
   held one's address, which is its end. Retaining it would make every stack-held object keep its
   neighbour alive. */
#include "krusty_rt.h"
#include "krusty_sys.h"

static const KType bytes_type = {
    .name = "Bytes",
    .name_length = 5,
    .instance_size = sizeof(KArray),
    .element_size = 1,
};
static const KType blob_type = {.name = "Blob", .name_length = 4};

#define SMALL 32u
#define LARGE 4096u

__attribute__((noinline)) static uintptr_t end_of_new_array(uint32_t size) {
    uint8_t *object = (uint8_t *)kt_gc_allocate(&bytes_type, size);
    ((KArray *)object)->length = (kt_int)(size - sizeof(KArray));
    return (uintptr_t)(object + size);
}

/* Two plain objects of one size class, allocated back to back; only the second is returned. */
__attribute__((noinline)) static uintptr_t second_of_two_blobs(void) {
    uint8_t *first = (uint8_t *)kt_gc_allocate(&blob_type, 16);
    uint8_t *second = (uint8_t *)kt_gc_allocate(&blob_type, 16);
    (void)first;
    return (uintptr_t)second;
}

__attribute__((noinline)) static void collect(void) {
    volatile uintptr_t words[4096];
    for (unsigned i = 0; i < 4096; i++) {
        words[i] = 0;
    }
    (void)words[0];
    kt_gc_collect();
}

__attribute__((noinline)) static void check_end_pointers(void) {
    volatile uintptr_t small_end = end_of_new_array(SMALL);
    volatile uintptr_t large_end = end_of_new_array(LARGE);
    collect();
    if (kt_gc_live_objects() != 2) {
        KT_SYS_FAIL("an array only an end pointer reaches was freed\n");
    }
    const KObjectHeader *small = (const KObjectHeader *)(small_end - SMALL);
    const KObjectHeader *large = (const KObjectHeader *)(large_end - LARGE);
    if (small->type != &bytes_type || large->type != &bytes_type) {
        KT_SYS_FAIL("an array only an end pointer reaches was overwritten\n");
    }
    volatile uintptr_t held = second_of_two_blobs();
    collect();
    if (kt_gc_live_objects() != 3) {
        KT_SYS_FAIL("a held object kept the plain object before it alive\n");
    }
    if (((const KObjectHeader *)held)->type != &blob_type) {
        KT_SYS_FAIL("a held object was overwritten\n");
    }
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    check_end_pointers();
    kt_sys_write(1, "OK\n", 3);
}
