/* The members that ask one program call and answer from it -- `IndexedValue`'s `equals` and
   `hashCode`, `none { }`, and `none()`/`isEmpty()` over a program's `Iterable` -- stop at a call
   that throws: that is the last call made, its exception is the one pending, and nothing further
   is computed from what it returned. They used to fold or negate the placeholder into an answer
   first; a caller never reads that answer beside a pending exception, so this driver checks what
   a program can see: the calls and the exception. */
#include "program_collections.h"

static KRef throw_predicate(KRef self, KRef element) {
    (void)self;
    (void)element;
    log_text("f ");
    throw_fresh();
    return NULL;
}

FUNCTION_VALUE(f_throws, throw_predicate)

void kt_program_entry(void) {
    PROGRAM_BEGIN();
    KRef quiet = tag(1, "");

    (void)kt_equals(kt_indexed_value(0, tag(1, "e")), kt_indexed_value(0, quiet));
    CHECK(threw_last() && log_is("eq(1) "), "IndexedValue.equals\n");
    (void)kt_equals(kt_indexed_value(0, tag(1, "e")), kt_indexed_value(1, quiet));
    CHECK(kt_pending_exception() == NULL && log_is(""),
          "IndexedValue.equals asked the value although the indices differ\n");
    (void)kt_hash_code(kt_indexed_value(3, tag(2, "h")));
    CHECK(threw_last() && log_is("hash(2) "), "IndexedValue.hashCode\n");

    KRef elements[] = {quiet};
    (void)kt_iterable_none(seq_of(&seq_type, elements, 1), f_throws);
    CHECK(threw_last() && log_is("iterator hasNext next f "), "none { }\n");
    for (int placeholder = 0; placeholder <= 1; placeholder++) {
        KRef seq = seq_throwing(seq_of(&seq_type, elements, 1), "h", 0, placeholder);
        (void)kt_iterable_is_empty(seq);
        CHECK(threw_last() && log_is("iterator hasNext "), "none() with a throwing hasNext\n");
        seq = seq_throwing(seq_of(&seq_type, elements, 1), "i", 0, placeholder);
        (void)kt_iterable_is_empty(seq);
        CHECK(threw_last() && log_is("iterator "), "none() with a throwing iterator\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
