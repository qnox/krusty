/* `print(x)` and `println(x)` whose `toString` raises write NOTHING and leave the program's
   exception in flight. Kotlin renders the value before it writes, so a raise in the rendering ends
   the call there. The runtime used to write the `null` its renderer falls back to -- and for
   `println` the newline after it -- from a call that should only have propagated.

   The harness requires this driver's whole output to be the `OK` it prints last, so any byte
   either call wrote fails the test. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#define CHECK(condition, what)                                                                     \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL(what "\n");                                                                \
        }                                                                                          \
    } while (0)

/* The exception the program's `toString` raised last, compared by identity; a root, for the reason
   `user_code_raise_keeps_first_exception` gives. */
static KRef raised_by_program;

static KRef raising_to_string(KRef self) {
    (void)self;
    raised_by_program = kt_throwable_new(&kt_type_illegal_state_exception, NULL);
    kt_throw(raised_by_program);
    return NULL;
}

static const kt_fn unprintable_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                           (kt_fn)raising_to_string};

static const KType unprintable_type = {
    .name = "Unprintable",
    .name_length = sizeof("Unprintable") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = unprintable_vtable,
    .vtable_length = 3,
};

static kt_boolean program_exception_pending(void) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    kt_boolean kept = thrown != NULL && thrown == raised_by_program;
    raised_by_program = NULL;
    return kept;
}

__attribute__((noinline)) static void run(KRef unprintable) {
    kt_print_any(unprintable);
    CHECK(program_exception_pending(), "print whose toString threw lost the exception");
    kt_println_any(unprintable);
    CHECK(program_exception_pending(), "println whose toString threw lost the exception");
}

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    kt_gc_add_global_root((void **)&raised_by_program);
    run(kt_gc_allocate(&unprintable_type, sizeof(KObjectHeader)));
    kt_sys_write(1, "OK\n", 3);
}
