/* What a driver that raises and catches needs on top of `driver_checks.h`: taking the exception in
   flight and asking what it is and said, and a function value to hand the runtime. Include this,
   and not `driver_checks.h` beside it, from exactly one file per driver. */
#ifndef KRUSTY_DRIVER_EXCEPTIONS_H
#define KRUSTY_DRIVER_EXCEPTIONS_H

#include "driver_checks.h"

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
