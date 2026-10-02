/* What a driver of the list and iteration runtime needs from tiers above it, on top of the
   exception slot `later_tiers.h` stands in for: the exceptions a list raises, and `equals` and
   `hashCode`.

   Every definition is WEAK for the reason `later_tiers.h` gives: the tier that defines the real one
   wins the link, and the driver then runs against it unchanged. Each keeps the real contract and no
   more. Include this, and not `later_tiers.h` beside it, from exactly one file per driver. */
#ifndef KRUSTY_TEST_COLLECTIONS_LATER_TIERS_H
#define KRUSTY_TEST_COLLECTIONS_LATER_TIERS_H

#include "later_tiers.h"

/* The exceptions a list, a map or a set raises that `later_tiers.h` does not already stand in for,
   laid out as its throwable is, so a pending one keeps its message. */
#define STANDIN_EXCEPTION(identifier, kotlin_name)                                                 \
    __attribute__((weak)) const KType identifier = {                                               \
        .name = kotlin_name,                                                                       \
        .name_length = sizeof(kotlin_name) - 1,                                                    \
        .instance_size = sizeof(DriverThrowable),                                                  \
        .reference_count = sizeof(driver_throwable_offsets) / sizeof(driver_throwable_offsets[0]), \
        .reference_offsets = driver_throwable_offsets,                                             \
        .super = &kt_type_any,                                                                     \
    };

STANDIN_EXCEPTION(kt_type_no_such_element_exception, "kotlin.NoSuchElementException")
STANDIN_EXCEPTION(kt_type_concurrent_modification_exception,
                  "kotlin.ConcurrentModificationException")
STANDIN_EXCEPTION(kt_type_unsupported_operation_exception, "kotlin.UnsupportedOperationException")
STANDIN_EXCEPTION(kt_type_class_cast_exception, "kotlin.ClassCastException")

/* `kt_equals` and `kt_hash_code` come from `later_tiers.h`, which this header includes. */

/* A boxed `UInt`, which a later tier boxes: its bits in the `Int` field, under its own descriptor. */
__attribute__((weak)) KRef kt_box_uint(kt_int value) {
    DriverValue *box = (DriverValue *)kt_gc_allocate(&kt_type_uint, sizeof(DriverValue));
    box->as.int_value = value;
    return (KRef)box;
}

/* Take the exception in flight off the slot and say whether it has the expected type and exactly
   the expected message (NULL for none). */
static inline kt_boolean took_message(const KType *expected, const char *message) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    if (thrown == NULL || type_of(thrown) != expected) {
        return 0;
    }
    KRef text = kt_throwable_message(thrown);
    if (message == NULL || text == NULL) {
        return message == NULL && text == NULL;
    }
    kt_int length = 0;
    while (message[length] != 0) {
        length++;
    }
    return text_is(text, message, length);
}

/* Take the exception in flight off the slot and say whether it has the expected type. NULL, when
   nothing is in flight, has none. */
static inline kt_boolean took(const KType *expected) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    return thrown != NULL && type_of(thrown) == expected;
}

/* A function value: an object whose descriptor puts `invoke` in the one slot a function value
   declares beyond `kotlin.Any`'s three. `IDENTIFIER_type` is the descriptor and `IDENTIFIER` an
   instance of it, which needs no fields because every function here keeps its state in statics. */
#define FUNCTION_VALUE(identifier, invoke)                                                         \
    static const kt_fn identifier##_vtable[] = {NULL, NULL, NULL, (kt_fn)(invoke)};               \
    static const KType identifier##_type = {                                                       \
        .name = #identifier,                                                                       \
        .name_length = sizeof(#identifier) - 1,                                                    \
        .instance_size = sizeof(KObjectHeader),                                                    \
        .super = &kt_type_any,                                                                     \
        .vtable = identifier##_vtable,                                                             \
        .vtable_length = 4,                                                                        \
    };                                                                                             \
    static KObjectHeader identifier##_object = {&identifier##_type};                               \
    static KRef const identifier = (KRef)&identifier##_object;

#endif
