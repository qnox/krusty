/* What the walk-counter drivers share: `Many`, a program `Iterable` of `left` elements that counts
   down as they are read, and the runtime's seam for starting every walk's counter.

   Kotlin's walks count with an `Int` and raise at 2^31 (`checkCountOverflow`,
   `checkIndexOverflow`), which takes 2^31 elements to reach. Every runtime walk starts its counter
   at `kt_walk_counter_origin` and checks it through one helper, so a driver that starts the
   counters at 2^31 - k reaches the check after k elements, exactly where a walk of 2^31 - k more
   elements would. The JVM cannot walk 2^31 elements within the harness's limit on a loaded
   machine, so the Kotlin program beside each driver records kotlinc's answers for those walks and
   runs only short ones, which the transcripts compare. Include this from exactly one file per
   driver. */
#ifndef KRUSTY_TEST_WALK_COUNTER_H
#define KRUSTY_TEST_WALK_COUNTER_H

#include "collections_later_tiers.h"
#include "transcript.h"

extern uint32_t kt_walk_counter_origin;

typedef struct Many {
    KObjectHeader header;
    int64_t left;
} Many;

typedef struct ManyIterator {
    KObjectHeader header;
    KRef many;
} ManyIterator;

static KObjectHeader unit_object;

static kt_boolean many_has_next(KRef self) {
    return ((const Many *)((const ManyIterator *)self)->many)->left > 0;
}

static KRef many_next(KRef self) {
    ((Many *)((const ManyIterator *)self)->many)->left--;
    return (KRef)&unit_object;
}

static const uint32_t many_iterator_offsets[] = {offsetof(ManyIterator, many)};

static const KType many_iterator_type = {
    .name = "ManyIterator",
    .name_length = sizeof("ManyIterator") - 1,
    .instance_size = sizeof(ManyIterator),
    .reference_count = 1,
    .reference_offsets = many_iterator_offsets,
    .super = &kt_type_any,
    .walk_has_next = many_has_next,
    .walk_next = many_next,
};

static KRef many_iterator(KRef self) {
    ManyIterator *iterator =
        (ManyIterator *)kt_gc_allocate(&many_iterator_type, sizeof(ManyIterator));
    iterator->many = self;
    return (KRef)iterator;
}

static const KType many_type = {
    .name = "Many",
    .name_length = sizeof("Many") - 1,
    .instance_size = sizeof(Many),
    .super = &kt_type_any,
    .walk_iterator = many_iterator,
};

static KRef many(int64_t left) {
    Many *made = (Many *)kt_gc_allocate(&many_type, sizeof(Many));
    made->left = left;
    return (KRef)made;
}

/* `label: answer, <left> left`, the answer being `answer` or what the walk raised. */
static void show_walk(const char *label, kt_int answer, KRef walked) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    say(label);
    say(": ");
    if (thrown != NULL) {
        say_thrown(thrown);
    } else {
        say_long(answer);
    }
    say(", ");
    say_long(((const Many *)walked)->left);
    say(" left\n");
}

/* The exception pending is Kotlin's `message` overflow; the slot is cleared. */
static kt_boolean raised_overflow(const char *message) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    kt_int length = 0;
    while (message[length] != 0) {
        length++;
    }
    return thrown != NULL && type_of(thrown) == &kt_type_arithmetic_exception &&
           text_is(kt_throwable_message(thrown), message, length);
}

#define WALK_COUNTER_BEGIN()                                                                       \
    DRIVER_BEGIN();                                                                                \
    unit_object.type = &kt_type_any

#endif
