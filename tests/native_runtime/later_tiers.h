/* Stand-ins for the runtime a LATER tier defines, so a driver can reach code that raises before
   the tier that implements raising has landed.

   The runtime lands in tiers, and one below the last is linked with its missing symbols left
   unresolved: a driver that calls one jumps to address zero. Exceptions are the case that matters,
   because the rule every raise site is held to -- `kt_throw` records the exception and COMES BACK,
   and the caller polls the pending slot -- is exactly what a driver must observe, and at this tier
   nothing records anything.

   Every definition here is WEAK, so the tier that supplies the real one wins the link and the
   driver then runs against it unchanged. Each stand-in keeps the real contract (`krusty_rt.h`) and
   no more: the slot is a collector root, a thrown object carries its type, and `toString` goes
   through the vtable slot `kotlin.Any` declares. Include this from exactly one file per driver. */
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

/* Only the type is kept: a driver asks what was thrown, never what it said. */
__attribute__((weak)) KRef kt_throwable_new(const KType *type, KRef message) {
    (void)message;
    return (KRef)kt_gc_allocate(type, sizeof(KObjectHeader));
}

__attribute__((weak)) const KType kt_type_null_pointer_exception = {
    .name = "kotlin.NullPointerException",
    .name_length = sizeof("kotlin.NullPointerException") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
};

/* The exceptions the stdlib throwers raise. A driver tells them apart by descriptor, so each is a
   distinct object; the real ones name their `Throwable` superclass, which no driver asks for. */
#define LATER_TIERS_EXCEPTION(identifier, kotlin_name)                                             \
    __attribute__((weak)) const KType identifier = {                                               \
        .name = kotlin_name,                                                                       \
        .name_length = sizeof(kotlin_name) - 1,                                                    \
        .instance_size = sizeof(KObjectHeader),                                                    \
        .super = &kt_type_any,                                                                     \
    };

LATER_TIERS_EXCEPTION(kt_type_illegal_state_exception, "kotlin.IllegalStateException")
LATER_TIERS_EXCEPTION(kt_type_assertion_error, "kotlin.AssertionError")
LATER_TIERS_EXCEPTION(kt_type_not_implemented_error, "kotlin.NotImplementedError")

#undef LATER_TIERS_EXCEPTION

/* The runtime declares this one `static` and defines it in a later tier; until then its call is an
   unresolved reference like any other, and this answers it the way the real one does. */
__attribute__((weak)) KRef kt_object_to_string(KRef value) {
    const KType *type = type_of(value);
    if (type->vtable == NULL || type->vtable_length <= KT_SLOT_TO_STRING) {
        return kt_any_to_string(value);
    }
    return ((KRef(*)(KRef))type->vtable[KT_SLOT_TO_STRING])(value);
}

/* Whether `text` -- a string or a builder -- holds exactly these BYTES. Two byte-wise prefix tests
   rather than an equality, because the object layout is the runtime's own and `compareTo` decodes
   to UTF-16 units, which would call two different encodings of a character equal. */
static inline kt_boolean text_is(KRef text, const char *bytes, kt_int byte_length) {
    KRef expected = kt_string_utf8(bytes, byte_length);
    return kt_string_starts_with(text, expected) && kt_string_starts_with(expected, text);
}

#endif
