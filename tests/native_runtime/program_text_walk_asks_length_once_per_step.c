/* A `CharSequence` the PROGRAM implements is walked the way Kotlin's own `CharIterator` walks it:
   `hasNext` asks `length`, and `next` is `get(index++)` alone. The walk used to ask `length` again
   inside every `next`, so a program counting calls to its getter saw two per character — Kotlin's
   own `forInCharSequenceWithIndexCheckSideEffects` counts exactly that.

   Both protocols are walked over three characters: each asks `length` four times (three steps and
   the one that ends the walk) and `get` three times. */
#include "collections_later_tiers.h"

typedef struct Counting {
    KObjectHeader header;
    kt_int length_calls;
    kt_int get_calls;
} Counting;

static const char letters[] = "abc";

static kt_int counting_length(KRef self) {
    ((Counting *)self)->length_calls++;
    return 3;
}

static kt_char counting_char_at(KRef self, kt_int index) {
    ((Counting *)self)->get_calls++;
    return (kt_char)letters[index];
}

static const KType counting_type = {
    .name = "Counting",
    .name_length = 8,
    .instance_size = sizeof(Counting),
    .walk_length = counting_length,
    .walk_char_at = counting_char_at,
};

static Counting *new_counting(void) {
    Counting *text = (Counting *)kt_gc_allocate(&counting_type, sizeof(Counting));
    text->length_calls = 0;
    text->get_calls = 0;
    return text;
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);

    Counting *general = new_counting();
    KRef walk = kt_iterable_iterator((KRef)general);
    for (int at = 0; kt_iterator_has_next(walk); at++) {
        CHECK(kt_unbox_char(kt_iterator_next(walk)) == (kt_char)letters[at],
              "the general walk yielded the wrong character\n");
    }
    CHECK(general->length_calls == 4, "the general walk asked length more than once per step\n");
    CHECK(general->get_calls == 3, "the general walk read a character more than once\n");

    Counting *narrow = new_counting();
    walk = kt_iterable_iterator((KRef)narrow);
    for (int at = 0; kt_range_iterator_has_next(walk); at++) {
        CHECK((kt_char)kt_range_iterator_next(walk) == (kt_char)letters[at],
              "the narrow walk yielded the wrong character\n");
    }
    CHECK(narrow->length_calls == 4, "the narrow walk asked length more than once per step\n");
    CHECK(narrow->get_calls == 3, "the narrow walk read a character more than once\n");
    CHECK(kt_pending_exception() == NULL, "a walk raised\n");
    kt_sys_write(1, "OK\n", 3);
}
