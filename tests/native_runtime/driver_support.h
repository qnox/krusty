/* What every driver shares: the layouts the runtime keeps private that a driver must read or build,
   and the start every driver makes.

   The runtime is complete, and a driver links against all of it: nothing here stands in for a
   runtime function. The string layout mirrors `struct KObject`'s string arm, which only
   `krusty_internal.h` declares; a driver has no other way to read the bytes a runtime call hands
   back, since the runtime's own text accessor has internal linkage. The builder layout mirrors
   `KStringBuilder`, so a driver can hand the runtime a builder of exact contents, and the
   throwable layout mirrors `KThrowable`, so a driver can declare a subclass of `Throwable` as a
   program does. */
#ifndef KRUSTY_DRIVER_SUPPORT_H
#define KRUSTY_DRIVER_SUPPORT_H

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

static inline const KType *driver_type_of(KRef value) {
    return ((const KObjectHeader *)value)->type;
}

/* A `StringBuilder` as the runtime lays one out (`KStringBuilder`): the byte array holding its
   UTF-8 text, whose body follows the array's header, and how many of those bytes are text. */
typedef struct DriverStringBuilder {
    KObjectHeader header;
    KRef storage;
    kt_int byte_length;
} DriverStringBuilder;

static inline char *driver_builder_bytes(KRef builder) {
    return (char *)((KArray *)((DriverStringBuilder *)builder)->storage + 1);
}

/* A builder holding a copy of the `length` bytes at `bytes`. */
static inline KRef driver_builder_of(const char *bytes, kt_int length) {
    KRef storage = kt_array_new(&kt_type_byte_array, length);
    DriverStringBuilder *builder = (DriverStringBuilder *)kt_gc_allocate(
        &kt_type_string_builder, sizeof(DriverStringBuilder));
    builder->storage = storage;
    builder->byte_length = length;
    char *body = driver_builder_bytes((KRef)builder);
    for (kt_int index = 0; index < length; index++) {
        body[index] = bytes[index];
    }
    return (KRef)builder;
}

/* The text of a string or a builder, the two shapes a driver reads: a builder's is its array's
   body, a string's its own. */
static inline const char *driver_text_of(KRef self, kt_int *byte_length) {
    if (driver_type_of(self) == &kt_type_string_builder) {
        *byte_length = ((const DriverStringBuilder *)self)->byte_length;
        return driver_builder_bytes(self);
    }
    return driver_text(self, byte_length);
}

/* A thrown object as the runtime lays one out (`KThrowable`): its message, then its cause. A
   driver declaring a subclass of `Throwable` sizes it and lists its references from these. */
typedef struct DriverThrowable {
    KObjectHeader header;
    KRef message;
    KRef cause;
} DriverThrowable;

static const uint32_t driver_throwable_offsets[] = {offsetof(DriverThrowable, message),
                                                    offsetof(DriverThrowable, cause)};

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

#endif /* KRUSTY_DRIVER_SUPPORT_H */
