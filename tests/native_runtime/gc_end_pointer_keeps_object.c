/* A pointer one past the end of an object keeps that object alive. A C compiler walking an array
   may keep only its end pointer once the base is dead; when the object fills its slot exactly,
   that address is the next slot's start or past the chunk altogether, and the object used to be
   freed while the loop was still reading it.

   Both shapes are checked: a small object that fills its size class, and a large object whose size
   is a multiple of 16 so it fills its own chunk. The only copies left are the end pointers, in
   volatile locals, and the stack below is scrubbed so a stale base pointer cannot stand in. */
#include "krusty_rt.h"
#include "krusty_sys.h"

static const KType blob_type = {.name = "Blob", .name_length = 4};

#define SMALL 16u
#define LARGE 4096u

__attribute__((noinline)) static uintptr_t end_of_new(uint32_t size) {
    uint8_t *object = (uint8_t *)kt_gc_allocate(&blob_type, size);
    return (uintptr_t)(object + size);
}

__attribute__((noinline)) static void collect(void) {
    volatile uintptr_t words[4096];
    for (unsigned i = 0; i < 4096; i++) {
        words[i] = 0;
    }
    kt_gc_collect();
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    volatile uintptr_t small_end = end_of_new(SMALL);
    volatile uintptr_t large_end = end_of_new(LARGE);
    collect();
    if (kt_gc_live_objects() != 2) {
        KT_SYS_FAIL("an object only an end pointer reaches was freed\n");
    }
    const KObjectHeader *small = (const KObjectHeader *)(small_end - SMALL);
    const KObjectHeader *large = (const KObjectHeader *)(large_end - LARGE);
    if (small->type != &blob_type || large->type != &blob_type) {
        KT_SYS_FAIL("an object only an end pointer reaches was overwritten\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
