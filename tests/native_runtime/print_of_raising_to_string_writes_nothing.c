/* `print(x)` and `println(x)` whose `toString` raises write NOTHING and leave the program's
   exception in flight. Kotlin renders the value before it writes, so a raise in the rendering ends
   the call there. The runtime used to write the `null` its renderer falls back to -- and for
   `println` the newline after it -- from a call that should only have propagated.

   The driver prints what each call raised, and the harness compares the whole of its stdout with
   what `print_of_raising_to_string_writes_nothing.kt` answers under the reference kotlinc: a byte
   either call wrote would stand before the line that reports it. */
#include "transcript.h"

#define REQUIRE(condition, what)                                                                     \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL(what "\n");                                                                \
        }                                                                                          \
    } while (0)

/* The exception the program's `toString` raised last, compared by identity; a root, for the reason
   `user_code_raise_keeps_first_exception` gives. */
static KRef raised_by_program;

PROGRAM_EXCEPTION(boom_type, "Boom")

static KRef raising_to_string(KRef self) {
    (void)self;
    raised_by_program = kt_throwable_new(&boom_type, kt_string_utf8("boom", 4));
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

/* The transcript's line for a call that must have raised the program's own exception, which it
   checks by identity and takes, as the `catch` would. */
static void threw(const char *call) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    REQUIRE(thrown != NULL && thrown == raised_by_program, "the program's exception was lost");
    raised_by_program = NULL;
    say(call);
    say(" = threw ");
    say_bytes_of(((const KObjectHeader *)thrown)->type->simple_name,
                 ((const KObjectHeader *)thrown)->type->simple_name_length);
    say("\n");
}

__attribute__((noinline)) static void run(KRef unprintable) {
    kt_print_any(unprintable);
    threw("print");
    kt_println_any(unprintable);
    threw("println");
}

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    kt_gc_add_global_root((void **)&raised_by_program);
    run(kt_gc_allocate(&unprintable_type, sizeof(KObjectHeader)));
    kt_sys_write(1, "OK\n", 3);
}
