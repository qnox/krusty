/* The checks the drivers of the list, map and exception runtime make, on top of
   `driver_support.h`. Include this from exactly one file per driver. */
#ifndef KRUSTY_DRIVER_CHECKS_H
#define KRUSTY_DRIVER_CHECKS_H

#include "driver_support.h"

/* A failed expectation ends the driver with the message, which the harness reports. */
#define CHECK(condition, literal)                                                                  \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL(literal);                                                                  \
        }                                                                                          \
    } while (0)

/* An object's descriptor. The object layout is the runtime's own; the header is its public start. */
static inline const KType *type_of(KRef object) { return ((const KObjectHeader *)object)->type; }

/* Whether `text` -- a string or a builder -- holds exactly these BYTES. Read through the layouts
   `driver_support.h` mirrors rather than through a runtime comparison: `compareTo` and
   `startsWith` decode to UTF-16 units, which would call two different encodings of a character
   equal. */
static inline kt_boolean text_is(KRef text, const char *bytes, kt_int byte_length) {
    kt_int length = 0;
    const char *have = driver_text_of(text, &length);
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
