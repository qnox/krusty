/* A list whose next `add` would grow its storage past what an `Int` doubles to ends the program as
   out of memory, which is where Kotlin fails too. Its growth arithmetic used to double the capacity
   in `kt_int`: past half the largest `Int` the doubling overflowed and came out negative, and the
   `add` reported a negative array size instead -- or, where a wrapped size still passed, copied the
   elements into storage too small to hold them.

   The list is a STAND-IN built in static storage: a header that declares a full storage of 2^30
   elements over a storage array with no elements behind it at all. Nothing is allocated to build
   it, and nothing may read its payload: the fixed runtime refuses the growth before copying a
   single element, and the harness expects that refusal. This driver must NOT print OK. */
#include "later_tiers.h"

/* `KMutableList`'s layout, which `krusty_rt.c` keeps private; a driver has no other way to hand the
   runtime a list whose size it could never allocate. */
typedef struct DriverMutableList {
    KObjectHeader header;
    KRef elements;
    kt_int size;
    kt_int modifications;
} DriverMutableList;

/* 2^30: doubled, it is one past the largest `Int`. */
#define DECLARED_CAPACITY 0x40000000

static KArray storage = {{&kt_type_array}, DECLARED_CAPACITY};

static DriverMutableList list = {{&kt_type_mutable_list}, (KRef)&storage, DECLARED_CAPACITY, 0};

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    (void)kt_mutable_list_add((KRef)&list, NULL);
    KT_SYS_FAIL("an add past the largest capacity came back\n");
}
