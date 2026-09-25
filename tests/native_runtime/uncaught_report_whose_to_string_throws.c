/* An uncaught exception whose `toString` raises is reported as the JVM reports it: the report's
   opening, then a line naming the class of what the `toString` raised. There is no caller left to
   propagate that second exception to, and nothing it rendered is text to print -- the report used
   to write the `null` its renderer falls back to, as if that were the exception's text. */
#include "krusty_rt.h"
#include "krusty_sys.h"

typedef struct Unprintable {
    KObjectHeader header;
    KRef message;
    KRef cause;
} Unprintable;

static const uint32_t unprintable_offsets[] = {offsetof(Unprintable, message),
                                               offsetof(Unprintable, cause)};

static KRef raising_to_string(KRef self) {
    (void)self;
    kt_throw(kt_throwable_new(&kt_type_illegal_state_exception, NULL));
    return NULL;
}

static const kt_fn unprintable_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                           (kt_fn)raising_to_string};

static const KType unprintable_type = {
    .name = "Unprintable",
    .name_length = sizeof("Unprintable") - 1,
    .instance_size = sizeof(Unprintable),
    .reference_count = 2,
    .reference_offsets = unprintable_offsets,
    .super = &kt_type_runtime_exception,
    .vtable = unprintable_vtable,
    .vtable_length = 3,
};

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    kt_throw(kt_throwable_new(&unprintable_type, NULL));
    /* Ends the program with the report on stderr; the harness reads it. */
    kt_check_uncaught();
    kt_sys_write(1, "the uncaught exception did not end the program\n", 47);
}
