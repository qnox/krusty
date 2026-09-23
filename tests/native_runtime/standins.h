/* What a driver needs from tiers of the runtime above the one it runs against.

   The runtime lands in tiers, and a tier below the last one calls functions a later tier defines:
   the text accessor behind every question about a string's content, the boxes an exception message
   renders, and the exception machinery itself. Linked at such a tier those calls reach nothing, so a
   driver that exercises code making them supplies the missing pieces here. Every one is WEAK: at a
   tier that defines the real function the linker takes that one, and the driver runs against the
   runtime as it will ship. `kt_text_of` is the exception — the runtime defines it with internal
   linkage, so from the tier that does, its calls never reach this definition.

   The string layout below mirrors `struct KObject`'s string arm in `krusty_rt.c`, which keeps the
   layout private; a driver has no other way to read the text a runtime call hands back. */
#ifndef KRUSTY_DRIVER_STANDINS_H
#define KRUSTY_DRIVER_STANDINS_H

#include "krusty_rt.h"
#include "krusty_sys.h"

typedef struct DriverValue {
    KObjectHeader header;
    union {
        struct {
            KRef storage;
            const char *bytes;
            kt_int byte_length;
        } string;
        kt_char char_value;
        kt_int int_value;
    } as;
} DriverValue;

/* A string's UTF-8 bytes, with their count written to `byte_length`. */
static inline const char *driver_text(KRef string, kt_int *byte_length) {
    const DriverValue *value = (const DriverValue *)string;
    *byte_length = value->as.string.byte_length;
    return value->as.string.bytes;
}

/* Whether `string` holds exactly the `length` bytes at `expected`. */
static inline kt_boolean driver_text_is(KRef string, const char *expected, kt_int length) {
    kt_int byte_length = 0;
    const char *bytes = driver_text(string, &byte_length);
    if (byte_length != length) {
        return 0;
    }
    for (kt_int index = 0; index < length; index++) {
        if (bytes[index] != expected[index]) {
            return 0;
        }
    }
    return 1;
}

/* The exception in flight, taken off the slot; NULL when there is none. */
static inline KRef driver_take_pending(void) {
    KRef thrown = kt_pending;
    kt_pending = NULL;
    return thrown;
}

static inline const KType *driver_type_of(KRef value) { return ((const KObjectHeader *)value)->type; }

/* The stand-ins. A string is the only text a driver hands the runtime, so the text accessor reads
   only that shape. */
__attribute__((weak)) const char *kt_text_of(KRef self, kt_int *byte_length) {
    return driver_text(self, byte_length);
}

__attribute__((weak)) KRef kt_box_char(kt_char value) {
    DriverValue *box = (DriverValue *)kt_gc_allocate(&kt_type_char, sizeof(DriverValue));
    box->as.char_value = value;
    return (KRef)box;
}

__attribute__((weak)) KRef kt_box_int(kt_int value) {
    DriverValue *box = (DriverValue *)kt_gc_allocate(&kt_type_int, sizeof(DriverValue));
    box->as.int_value = value;
    return (KRef)box;
}

__attribute__((weak)) KRef kt_pending;

__attribute__((weak)) const KType kt_type_index_out_of_bounds_exception = {
    .name = "kotlin.IndexOutOfBoundsException",
    .name_length = sizeof("kotlin.IndexOutOfBoundsException") - 1,
    .instance_size = sizeof(KObjectHeader),
};

/* An exception here is its type and nothing else: that is all a driver asks of one. It is not
   collected, since the pending slot is not a root below the tier that makes it one, so it is
   allocated from static storage rather than the heap. */
__attribute__((weak)) KRef kt_throwable_new(const KType *type, KRef message) {
    static KObjectHeader thrown[64];
    static unsigned count;
    (void)message;
    if (count == sizeof(thrown) / sizeof(thrown[0])) {
        KT_SYS_FAIL("driver: more exceptions than the stand-in holds\n");
    }
    thrown[count].type = type;
    return (KRef)&thrown[count++];
}

__attribute__((weak)) void kt_throw(KRef thrown) { kt_pending = thrown; }

/* The collector finds roots from here up; a driver that allocates records it first. */
#define DRIVER_BEGIN()                                                                             \
    int driver_stack_bottom;                                                                       \
    kt_runtime_init(&driver_stack_bottom)

#define DRIVER_CHECK(condition, literal)                                                           \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL("driver: " literal "\n");                                                  \
        }                                                                                          \
    } while (0)

#endif /* KRUSTY_DRIVER_STANDINS_H */
