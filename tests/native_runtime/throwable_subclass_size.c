/* `kt_throwable_new` allocates the size the DESCRIPTOR states. A subclass of `Throwable` that adds
   fields of its own has a larger instance than the runtime's `message` and `cause`, and the object
   used to be allocated at the base size — so writing the subclass's fields overran it into the next
   object on the heap. */
#include "krusty_rt.h"
#include "krusty_sys.h"

/* The runtime's two fields, then three of the subclass's own. */
typedef struct Failure {
    KObjectHeader header;
    KRef message;
    KRef cause;
    kt_long code;
    kt_long line;
    kt_long column;
} Failure;

static const uint32_t failure_offsets[] = {offsetof(Failure, message), offsetof(Failure, cause)};

static const KType failure_type = {
    .name = "Failure",
    .name_length = 7,
    .instance_size = sizeof(Failure),
    .reference_count = 2,
    .reference_offsets = failure_offsets,
    .super = &kt_type_exception,
    .vtable = NULL,
    .vtable_length = 0,
};

__attribute__((noinline)) static void run(void) {
    Failure *first = (Failure *)kt_throwable_new(&failure_type, kt_string_utf8("first", 5));
    Failure *second =
        (Failure *)kt_throwable_new_with_cause(&failure_type, kt_string_utf8("second", 6), NULL);
    first->code = 1;
    first->line = 2;
    first->column = 3;
    second->code = 4;
    second->line = 5;
    second->column = 6;
    if (second->header.type != &failure_type || first->header.type != &failure_type) {
        KT_SYS_FAIL("a subclass's own fields overwrote the next object's header\n");
    }
    if (!kt_equals(kt_throwable_message((KRef)second), kt_string_utf8("second", 6)) ||
        !kt_equals(kt_throwable_message((KRef)first), kt_string_utf8("first", 5))) {
        KT_SYS_FAIL("a subclass's own fields overwrote the next object's message\n");
    }
    if (first->code != 1 || first->line != 2 || first->column != 3 || second->code != 4 ||
        second->line != 5 || second->column != 6) {
        KT_SYS_FAIL("a subclass's own fields did not keep what was written to them\n");
    }
}

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    run();
    kt_sys_write(1, "OK\n", 3);
}
