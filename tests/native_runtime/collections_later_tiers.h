/* What a driver of the list and iteration runtime needs from tiers above it, on top of the
   exception slot `later_tiers.h` stands in for: the exceptions a list raises, the set and map
   questions every walk asks before it reaches a range, and `hashCode`.

   Every definition is WEAK for the reason `later_tiers.h` gives: the tier that defines the real one
   wins the link, and the driver then runs against it unchanged. Each keeps the real contract and no
   more. Include this, and not `later_tiers.h` beside it, from exactly one file per driver. */
#ifndef KRUSTY_TEST_COLLECTIONS_LATER_TIERS_H
#define KRUSTY_TEST_COLLECTIONS_LATER_TIERS_H

#include "later_tiers.h"

#define STANDIN_EXCEPTION(identifier, kotlin_name)                                                 \
    __attribute__((weak)) const KType identifier = {                                               \
        .name = kotlin_name,                                                                       \
        .name_length = sizeof(kotlin_name) - 1,                                                    \
        .instance_size = sizeof(KObjectHeader),                                                    \
        .super = &kt_type_any,                                                                     \
    };

STANDIN_EXCEPTION(kt_type_no_such_element_exception, "kotlin.NoSuchElementException")
STANDIN_EXCEPTION(kt_type_index_out_of_bounds_exception, "kotlin.IndexOutOfBoundsException")
STANDIN_EXCEPTION(kt_type_concurrent_modification_exception,
                  "kotlin.ConcurrentModificationException")
STANDIN_EXCEPTION(kt_type_illegal_argument_exception, "kotlin.IllegalArgumentException")

/* Nothing a driver here builds is a set or a map, and those types do not exist at this tier, so the
   real answer for every receiver a driver can pass is `false`. */
__attribute__((weak)) kt_boolean kt_is_set(KRef value) {
    (void)value;
    return 0;
}

__attribute__((weak)) kt_boolean kt_is_map(KRef value) {
    (void)value;
    return 0;
}

/* The real one: zero for null, otherwise the `hashCode` slot. */
__attribute__((weak)) kt_int kt_hash_code(KRef value) {
    if (value == NULL) {
        return 0;
    }
    return ((kt_int(*)(KRef))type_of(value)->vtable[KT_SLOT_HASH_CODE])(value);
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
