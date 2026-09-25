/* `sb.appendLine(obj)` where `obj.toString()` throws: the exception propagates and the builder is
   left as it was, newline included. `appendLine` hands the value to `append`, which stops on the
   exception; the line used to go on regardless and add its newline, so the builder a program caught
   the exception around had changed. */
#include "later_tiers.h"

static KRef throwing_to_string(KRef self) {
    (void)self;
    kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    return NULL;
}

static const kt_fn throwing_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                        (kt_fn)throwing_to_string};

static const KType throwing_type = {
    .name = "Throwing",
    .name_length = sizeof("Throwing") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = throwing_vtable,
    .vtable_length = 3,
};

void kt_program_entry(void) {
    KRef builder = kt_string_builder_with_text(kt_string_utf8("kept", 4));
    KRef throwing = (KRef)kt_gc_allocate(&throwing_type, sizeof(KObjectHeader));

    (void)kt_string_builder_append_line(builder, throwing);
    CHECK(kt_pending_exception() != NULL, "the toString exception did not propagate\n");
    kt_clear_pending();
    CHECK(text_is(builder, "kept", 4), "a failed appendLine changed the builder\n");

    (void)kt_string_builder_append_line(builder, kt_string_utf8("!", 1));
    CHECK(text_is(builder, "kept!\n", 6), "the builder stopped taking lines\n");
    kt_sys_write(1, "OK\n", 3);
}
