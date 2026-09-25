/* The report of an uncaught exception runs the exception's own `toString` with NOTHING in flight.
   Generated code checks the pending slot after each call it makes, so a program's `toString`
   entered with the uncaught exception still in the slot saw it after its first call, took it for
   its own raise and came back with nothing -- and the report read `null` where the program's text
   belonged. */
#include "krusty_rt.h"
#include "krusty_sys.h"

/* A subclass of `Exception` whose `toString` is the program's: one call, then the check generated
   code makes after it. */
typedef struct Failure {
    KObjectHeader header;
    KRef message;
    KRef cause;
} Failure;

static const uint32_t failure_offsets[] = {offsetof(Failure, message), offsetof(Failure, cause)};

static KRef failure_to_string(KRef self) {
    KRef text = kt_string_plus(kt_string_utf8("Failure: ", 9), kt_throwable_message(self));
    if (kt_pending_exception() != NULL) {
        return NULL;
    }
    return text;
}

static const kt_fn failure_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                       (kt_fn)failure_to_string};

static const KType failure_type = {
    .name = "Failure",
    .name_length = sizeof("Failure") - 1,
    .instance_size = sizeof(Failure),
    .reference_count = 2,
    .reference_offsets = failure_offsets,
    .super = &kt_type_exception,
    .vtable = failure_vtable,
    .vtable_length = 3,
};

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    kt_throw(kt_throwable_new(&failure_type, kt_string_utf8("disk full", 9)));
    /* Ends the program with the report on stderr; the harness reads it. */
    kt_check_uncaught();
    kt_sys_write(1, "the uncaught exception did not end the program\n", 47);
}
