/* A spread copy that does not fit its destination raises `IndexOutOfBoundsException` and writes
   NOTHING. `kt_throw` records the exception and comes back, and the copy went on to `memcpy` past
   the destination's end anyway, overwriting whatever the heap held next; and its bound was
   `at + length` in 32-bit arithmetic, which overflows for an `at` near `Int.MAX_VALUE` and then
   passed the check. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#define CHECK(condition, literal)                                                                  \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL("array_copy_into_bounds: " literal "\n");                                  \
        }                                                                                          \
    } while (0)

/* The exception machinery a later tier defines. Until it lands these answer the calls the bounds
   check makes, keeping the real contract: a thrown object carries its type, and throwing records
   it in the pending slot and returns. They are WEAK, so the tier that defines the real ones wins
   the link and the driver then runs against it unchanged. */
__attribute__((weak)) KRef kt_pending;

__attribute__((weak)) void kt_throw(KRef thrown) { kt_pending = thrown; }

__attribute__((weak)) KRef kt_pending_exception(void) { return kt_pending; }

__attribute__((weak)) void kt_clear_pending(void) { kt_pending = NULL; }

/* Only the type is kept: the driver asks what was thrown, never what it said. It is not collected,
   since the pending slot is not a root below the tier that makes it one, so it comes from static
   storage rather than the heap. */
__attribute__((weak)) KRef kt_throwable_new(const KType *type, KRef message) {
    static KObjectHeader thrown[8];
    static unsigned count;
    (void)message;
    if (count == sizeof(thrown) / sizeof(thrown[0])) {
        KT_SYS_FAIL("array_copy_into_bounds: more exceptions than the stand-in holds\n");
    }
    thrown[count].type = type;
    return (KRef)&thrown[count++];
}

__attribute__((weak)) const KType kt_type_index_out_of_bounds_exception = {
    .name = "kotlin.IndexOutOfBoundsException",
    .name_length = sizeof("kotlin.IndexOutOfBoundsException") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
};

static kt_int *elements(KRef array) { return (kt_int *)((char *)array + sizeof(KArray)); }

static KRef filled(kt_int length, kt_int value) {
    KRef array = kt_array_new(&kt_type_int_array, length);
    for (kt_int index = 0; index < length; index++) {
        elements(array)[index] = value;
    }
    return array;
}

/* The copy must have thrown `IndexOutOfBoundsException`, and left the destination and the object
   the heap placed after it exactly as they were. */
static void expect_refused(KRef destination, KRef neighbour) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    CHECK(thrown != NULL, "a copy that does not fit threw nothing");
    CHECK(((const KObjectHeader *)thrown)->type == &kt_type_index_out_of_bounds_exception,
          "a copy that does not fit threw something other than IndexOutOfBoundsException");
    for (kt_int index = 0; index < 2; index++) {
        CHECK(elements(destination)[index] == 1, "a refused copy wrote into its destination");
    }
    for (kt_int index = 0; index < 64; index++) {
        CHECK(elements(neighbour)[index] == 2, "a refused copy wrote past its destination");
    }
}

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);

    KRef destination = filled(2, 1);
    KRef neighbour = filled(64, 2);
    KRef source = filled(4, 7);

    /* Too long for where it starts: the old code copied three elements past the end. */
    kt_array_copy_into(destination, 1, source);
    expect_refused(destination, neighbour);

    /* A start before the destination. */
    kt_array_copy_into(destination, -1, filled(1, 7));
    expect_refused(destination, neighbour);

    /* A start near `Int.MAX_VALUE`, where `at + length` wraps negative in 32 bits. */
    kt_array_copy_into(destination, INT32_MAX - 1, source);
    expect_refused(destination, neighbour);

    /* A copy that fits lands where it was asked and answers the index after it. */
    KRef two = filled(2, 9);
    KRef wide = filled(5, 0);
    CHECK(kt_array_copy_into(wide, 3, two) == 5, "the copy did not answer the index after it");
    CHECK(elements(wide)[2] == 0 && elements(wide)[3] == 9 && elements(wide)[4] == 9,
          "the copy did not land at its index");
    CHECK(kt_pending_exception() == NULL, "a copy that fits threw");

    kt_sys_write(1, "OK\n", 3);
}
