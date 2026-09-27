/* Stand-ins for the runtime a LATER tier defines, so a driver can reach code that raises before
   the tier that implements raising has landed.

   The runtime lands in tiers, and one below the last is linked with its missing symbols left
   unresolved: a driver that calls one jumps to address zero. Exceptions are the case that matters,
   because the rule every raise site is held to -- `kt_throw` records the exception and COMES BACK,
   and the caller polls the pending slot -- is exactly what a driver must observe, and at this tier
   nothing records anything.

   The exception slot, `kt_throw`, the throwable constructor and the `toString` dispatch are the
   ones `standins.h` already supplies to the string drivers; this header builds on that one rather
   than defining them a second time, so every stand-in has exactly one definition. What it adds is
   what only the drivers of this tier ask for. Every definition is WEAK, so the tier that supplies
   the real one wins the link and the driver then runs against it unchanged. Include this from
   exactly one file per driver. */
#ifndef KRUSTY_TEST_LATER_TIERS_H
#define KRUSTY_TEST_LATER_TIERS_H

#include "standins.h"

/* A failed expectation ends the driver with the message, which the harness reports. */
#define CHECK(condition, literal)                                                                  \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL(literal);                                                                  \
        }                                                                                          \
    } while (0)

/* An object's descriptor. The object layout is the runtime's own; the header is its public start. */
static inline const KType *type_of(KRef object) { return ((const KObjectHeader *)object)->type; }

__attribute__((weak)) void kt_clear_pending(void) { kt_pending = NULL; }

__attribute__((weak)) const KType kt_type_null_pointer_exception = {
    .name = "kotlin.NullPointerException",
    .name_length = sizeof("kotlin.NullPointerException") - 1,
    .instance_size = sizeof(DriverThrowable),
    .reference_count = sizeof(driver_throwable_offsets) / sizeof(driver_throwable_offsets[0]),
    .reference_offsets = driver_throwable_offsets,
    .super = &kt_type_any,
};

__attribute__((weak)) const KType kt_type_illegal_argument_exception = {
    .name = "kotlin.IllegalArgumentException",
    .name_length = sizeof("kotlin.IllegalArgumentException") - 1,
    .instance_size = sizeof(DriverThrowable),
    .reference_count = sizeof(driver_throwable_offsets) / sizeof(driver_throwable_offsets[0]),
    .reference_offsets = driver_throwable_offsets,
    .super = &kt_type_any,
};

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

/* Whether `text` -- a string or a builder -- holds exactly these BYTES. Read through the text
   accessor of `standins.h`, which mirrors both layouts, rather than through a runtime comparison:
   `compareTo` and `startsWith` decode to UTF-16 units, which would call two different encodings of
   a character equal. */
static inline kt_boolean text_is(KRef text, const char *bytes, kt_int byte_length) {
    kt_int length = 0;
    const char *have = kt_text_of(text, &length);
    if (length != byte_length) {
        return 0;
    }
    for (kt_int index = 0; index < length; index++) {
        if (have[index] != bytes[index]) {
            return 0;
        }
    }
    return 1;
}

#endif
