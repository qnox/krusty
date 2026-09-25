/* `StringBuilder(text)` where `text` is a `CharSequence` the PROGRAM implements and its `length`
   or `get` throws: no builder is made, the exception propagates from that call, and nothing more of
   the sequence is read. The construction used to carry on with the placeholder the aborted call
   returned: a placeholder LENGTH sized the builder, and a negative one raised a second exception
   over the first; a placeholder UNIT was appended and the next `get` asked, so program code Kotlin
   never reaches ran after the throw.

   Each member records the call and the exception it threw, which is how the driver proves the
   throwing call was the LAST call into the program and that its exception is the one pending. */
#include "later_tiers.h"

typedef struct Sequence {
    KObjectHeader header;
    /* The index whose `get` throws, or -1 when `length` itself throws. */
    kt_int throwing_index;
} Sequence;

static int calls;
static kt_int last_index;
static KRef last_thrown;

static void throw_now(void) {
    last_thrown = kt_throwable_new(&kt_type_null_pointer_exception, NULL);
    kt_throw(last_thrown);
}

/* Four units when it answers; a NEGATIVE placeholder when it throws, the one reading on would
   hand to the builder's capacity. */
static kt_int sequence_length(KRef self) {
    calls++;
    last_index = -1;
    if (((const Sequence *)self)->throwing_index < 0) {
        throw_now();
        return -1;
    }
    return 4;
}

static kt_char sequence_char_at(KRef self, kt_int index) {
    calls++;
    last_index = index;
    if (index == ((const Sequence *)self)->throwing_index) {
        throw_now();
        return 'x';
    }
    return (kt_char)('a' + index);
}

static const KType sequence_type = {
    .name = "Sequence",
    .name_length = sizeof("Sequence") - 1,
    .instance_size = sizeof(Sequence),
    .super = &kt_type_any,
    .walk_length = sequence_length,
    .walk_char_at = sequence_char_at,
};

static KRef sequence(kt_int throwing_index) {
    Sequence *made = (Sequence *)kt_gc_allocate(&sequence_type, sizeof(Sequence));
    made->throwing_index = throwing_index;
    return (KRef)made;
}

/* After one construction: no builder, the throwing call the last of `expected_calls` into the
   program, and that call's exception pending. */
static void expect_stopped_at(KRef builder, int expected_calls, kt_int throwing_index) {
    CHECK(builder == NULL, "a builder was made from a sequence that threw\n");
    CHECK(calls == expected_calls && last_index == throwing_index,
          "the sequence was read on after it threw\n");
    CHECK(kt_pending_exception() != NULL && kt_pending_exception() == last_thrown,
          "the sequence's exception is not the one pending\n");
    kt_clear_pending();
    calls = 0;
    last_index = 0;
    last_thrown = NULL;
}

void kt_program_entry(void) {
    /* `length` throws: it is the only call. */
    expect_stopped_at(kt_string_builder_with_text(sequence(-1)), 1, -1);
    /* `get(1)` throws: `length`, `get(0)`, `get(1)`, and nothing after. */
    expect_stopped_at(kt_string_builder_with_text(sequence(1)), 3, 1);

    kt_sys_write(1, "OK\n", 3);
}
