/* What a driver needs from tiers of the runtime above the one it runs against.

   The runtime lands in tiers, and a tier below the last one calls functions a later tier defines:
   the text accessor behind every question about a string's content, the `StringBuilder` it answers
   for, the dispatch that renders an object through its own `toString`, the boxes an exception
   message renders, and the exception machinery itself. Linked at such a tier those calls reach
   nothing, so a driver that exercises code making them supplies the missing pieces here. Every one
   is WEAK: at a tier that defines the real function the linker takes that one, and the driver runs
   against the runtime as it will ship. `kt_text_of` and `kt_object_to_string` are the exceptions —
   the runtime defines them with internal linkage, so from the tier that does, its calls never reach
   these definitions.

   The string layout below mirrors `struct KObject`'s string arm in `krusty_rt.c`, which keeps the
   layout private; a driver has no other way to read the text a runtime call hands back. The builder
   layout mirrors `KStringBuilder` in the tier that defines `StringBuilder`, for the same reason. */
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

/* A `StringBuilder` as the tier that defines it lays one out (`KStringBuilder` in `krusty_rt.c`
   there): the byte array holding its UTF-8 text, whose body follows the array's header, and how
   many of those bytes are text. The runtime below that tier already asks whether a receiver is one
   — a string cut from a builder must be a copy — so a driver builds one of exactly this shape to
   ask it. */
typedef struct DriverStringBuilder {
    KObjectHeader header;
    KRef storage;
    kt_int byte_length;
} DriverStringBuilder;

static inline char *driver_builder_bytes(KRef builder) {
    return (char *)((KArray *)((DriverStringBuilder *)builder)->storage + 1);
}

static const uint32_t driver_string_builder_offsets[] = {offsetof(DriverStringBuilder, storage)};

__attribute__((weak)) const KType kt_type_string_builder = {
    .name = "kotlin.text.StringBuilder",
    .name_length = sizeof("kotlin.text.StringBuilder") - 1,
    .instance_size = sizeof(DriverStringBuilder),
    .reference_count = 1,
    .reference_offsets = driver_string_builder_offsets,
    .super = &kt_type_any,
};

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

/* The stand-ins. The text accessor answers for the two shapes a driver hands the runtime, as the
   real one does: a builder's text is its array's body, and a string's is its own. */
__attribute__((weak)) const char *kt_text_of(KRef self, kt_int *byte_length) {
    if (driver_type_of(self) == &kt_type_string_builder) {
        *byte_length = ((const DriverStringBuilder *)self)->byte_length;
        return driver_builder_bytes(self);
    }
    return driver_text(self, byte_length);
}

/* An object's own `toString`, through the slot every vtable keeps for it, as the tier that defines
   the dispatch reaches it; a type with no such slot renders as `kotlin.Any` does. */
__attribute__((weak)) KRef kt_object_to_string(KRef value) {
    const KType *type = driver_type_of(value);
    if (type->vtable == NULL || type->vtable_length <= KT_SLOT_TO_STRING) {
        return kt_any_to_string(value);
    }
    return ((KRef(*)(KRef))type->vtable[KT_SLOT_TO_STRING])(value);
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

/* A thrown object as the runtime lays one out: its message, then its cause. A driver that asks
   what an exception SAID reads the message through `kt_throwable_message`, and the two stand-ins
   below agree on where it is, as the real pair does. */
typedef struct DriverThrowable {
    KObjectHeader header;
    KRef message;
    KRef cause;
} DriverThrowable;

static const uint32_t driver_throwable_offsets[] = {offsetof(DriverThrowable, message),
                                                    offsetof(DriverThrowable, cause)};

/* The exception in flight. `kt_throw` registers the slot as a collector root the first time it
   stores into it, as the real slot is one: while an exception is pending it, and its message, are
   reachable only through here. */
__attribute__((weak)) KRef kt_pending;

__attribute__((weak)) void kt_throw(KRef thrown) {
    static kt_boolean registered;
    if (!registered) {
        registered = 1;
        kt_gc_add_global_root((void **)&kt_pending);
    }
    kt_pending = thrown;
}

__attribute__((weak)) KRef kt_pending_exception(void) { return kt_pending; }

__attribute__((weak)) KRef kt_throwable_new(const KType *type, KRef message) {
    /* `message` stays in this parameter across the allocation: it is its root. */
    DriverThrowable *thrown = (DriverThrowable *)kt_gc_allocate(type, sizeof(DriverThrowable));
    thrown->message = message;
    thrown->cause = NULL;
    return (KRef)thrown;
}

__attribute__((weak)) KRef kt_throwable_message(KRef self) {
    return ((const DriverThrowable *)self)->message;
}

/* Every exception type a driver stands in for lists the throwable's fields for the collector, so a
   pending exception keeps its message alive. */
__attribute__((weak)) const KType kt_type_index_out_of_bounds_exception = {
    .name = "kotlin.IndexOutOfBoundsException",
    .name_length = sizeof("kotlin.IndexOutOfBoundsException") - 1,
    .instance_size = sizeof(DriverThrowable),
    .reference_count = sizeof(driver_throwable_offsets) / sizeof(driver_throwable_offsets[0]),
    .reference_offsets = driver_throwable_offsets,
    .super = &kt_type_any,
};

/* `kotlin.ArrayIndexOutOfBoundsException`, which reading a `String` past its ends throws, as a
   subclass of the one above. */
__attribute__((weak)) const KType kt_type_array_index_out_of_bounds_exception = {
    .name = "kotlin.ArrayIndexOutOfBoundsException",
    .name_length = sizeof("kotlin.ArrayIndexOutOfBoundsException") - 1,
    .instance_size = sizeof(DriverThrowable),
    .reference_count = sizeof(driver_throwable_offsets) / sizeof(driver_throwable_offsets[0]),
    .reference_offsets = driver_throwable_offsets,
    .super = &kt_type_index_out_of_bounds_exception,
};

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
