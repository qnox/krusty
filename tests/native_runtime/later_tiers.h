/* Stand-ins for the runtime a LATER tier defines, so a driver can reach code that raises before
   the tier that implements raising has landed.

   The runtime lands in tiers, and one below the last is linked with its missing symbols left
   unresolved: a driver that calls one jumps to address zero. Exceptions are the case that matters,
   because the rule every raise site is held to -- `kt_throw` records the exception and COMES BACK,
   and the caller polls the pending slot -- is exactly what a driver must observe, and at this tier
   nothing records anything.

   Every definition here is WEAK, so the tier that supplies the real one wins the link and the
   driver then runs against it unchanged. Each stand-in keeps the real contract (`krusty_rt.h`) and
   no more: the slot is a collector root, a thrown object carries its type and message, and
   `toString` goes through the vtable slot `kotlin.Any` declares. Include this from exactly one file
   per driver. */
#ifndef KRUSTY_TEST_LATER_TIERS_H
#define KRUSTY_TEST_LATER_TIERS_H

#include "krusty_rt.h"
#include "krusty_sys.h"

/* A failed expectation ends the driver with the message, which the harness reports. */
#define CHECK(condition, literal)                                                                  \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL(literal);                                                                  \
        }                                                                                          \
    } while (0)

/* An object's descriptor. The object layout is the runtime's own; the header is its public start. */
static inline const KType *type_of(KRef object) { return ((const KObjectHeader *)object)->type; }

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

__attribute__((weak)) void kt_clear_pending(void) { kt_pending = NULL; }

/* A thrown object as the runtime lays one out: its message, then its cause. A driver that asks
   what an exception SAID reads the message through `kt_throwable_message`, and the two stand-ins
   below agree on where it is, as the real pair does. */
typedef struct LaterThrowable {
    KObjectHeader header;
    KRef message;
    KRef cause;
} LaterThrowable;

static const uint32_t later_throwable_offsets[] = {offsetof(LaterThrowable, message),
                                                   offsetof(LaterThrowable, cause)};

__attribute__((weak)) KRef kt_throwable_new(const KType *type, KRef message) {
    /* `message` stays in this parameter across the allocation: it is its root. */
    LaterThrowable *thrown = (LaterThrowable *)kt_gc_allocate(type, sizeof(LaterThrowable));
    thrown->message = message;
    thrown->cause = NULL;
    return (KRef)thrown;
}

__attribute__((weak)) KRef kt_throwable_message(KRef self) {
    return ((const LaterThrowable *)self)->message;
}

__attribute__((weak)) const KType kt_type_null_pointer_exception = {
    .name = "kotlin.NullPointerException",
    .name_length = sizeof("kotlin.NullPointerException") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
};

/* Its message is text a driver reads back, so this one lists the fields for the collector: while
   it is pending, the message is reachable only through it. */
__attribute__((weak)) const KType kt_type_negative_array_size_exception = {
    .name = "java.lang.NegativeArraySizeException",
    .name_length = sizeof("java.lang.NegativeArraySizeException") - 1,
    .instance_size = sizeof(LaterThrowable),
    .reference_count = sizeof(later_throwable_offsets) / sizeof(later_throwable_offsets[0]),
    .reference_offsets = later_throwable_offsets,
    .super = &kt_type_any,
};

/* The runtime declares this one `static` and defines it in a later tier; until then its call is an
   unresolved reference like any other, and this answers it the way the real one does. */
__attribute__((weak)) KRef kt_object_to_string(KRef value) {
    const KType *type = type_of(value);
    if (type->vtable == NULL || type->vtable_length <= KT_SLOT_TO_STRING) {
        return kt_any_to_string(value);
    }
    return ((KRef(*)(KRef))type->vtable[KT_SLOT_TO_STRING])(value);
}

/* `a == b` and `a.hashCode()` as the real pair answer them: `null` equals only `null` and hashes
   to 0, and anything else answers through its own vtable slot -- which is where a program's
   override, and so a throw, comes in. */
__attribute__((weak)) kt_boolean kt_equals(KRef a, KRef b) {
    if (a == NULL) {
        return b == NULL;
    }
    const KType *type = type_of(a);
    if (type->vtable == NULL) {
        return a == b;
    }
    return ((kt_boolean(*)(KRef, KRef))type->vtable[KT_SLOT_EQUALS])(a, b);
}

__attribute__((weak)) kt_int kt_hash_code(KRef value) {
    if (value == NULL) {
        return 0;
    }
    const KType *type = type_of(value);
    if (type->vtable == NULL) {
        return kt_any_hash_code(value);
    }
    return ((kt_int(*)(KRef))type->vtable[KT_SLOT_HASH_CODE])(value);
}

/* Whether `text` -- a string or a builder -- holds exactly these BYTES. Two byte-wise prefix tests
   rather than an equality, because the object layout is the runtime's own and `compareTo` decodes
   to UTF-16 units, which would call two different encodings of a character equal. */
static inline kt_boolean text_is(KRef text, const char *bytes, kt_int byte_length) {
    KRef expected = kt_string_utf8(bytes, byte_length);
    return kt_string_starts_with(text, expected) && kt_string_starts_with(expected, text);
}

#endif
