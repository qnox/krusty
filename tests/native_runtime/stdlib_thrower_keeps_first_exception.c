/* `assert(false) { error("boom") }`, `error(x)` and `TODO(x)` whose message THROWS propagate that
   exception, as Kotlin's do: building the message is the first thing the call does, so what it
   raises is what the caller catches. The throwers used to raise their own exception over the one
   the message had already recorded, so a `catch` for the real exception missed. */
#include "later_tiers.h"

/* A `Function0` whose `invoke` raises, and an object whose `toString` raises -- each records a
   NullPointerException and comes back with nothing, as generated code does. */
static KRef raising_invoke(KRef self) {
    (void)self;
    kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    return NULL;
}

static KRef raising_to_string(KRef self) {
    (void)self;
    kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    return NULL;
}

static const kt_fn raising_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                       (kt_fn)raising_to_string, (kt_fn)raising_invoke};

static const KType raising_type = {
    .name = "Raising",
    .name_length = sizeof("Raising") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = raising_vtable,
    .vtable_length = 4,
};

static const KType *raised(void) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    return thrown == NULL ? NULL : type_of(thrown);
}

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);

    KRef raising = (KRef)kt_gc_allocate(&raising_type, sizeof(KObjectHeader));

    /* The forms whose message cannot throw still raise their own exception. */
    kt_assertion_failed(NULL);
    CHECK(raised() == &kt_type_assertion_error, "assert without a message did not raise\n");
    kt_illegal_state(kt_string_utf8("boom", 4));
    CHECK(raised() == &kt_type_illegal_state_exception, "error(\"boom\") did not raise\n");

    kt_assertion_failed(raising);
    CHECK(raised() == &kt_type_null_pointer_exception,
          "assert's lazy message threw, and a different exception replaced it\n");
    kt_illegal_state(raising);
    CHECK(raised() == &kt_type_null_pointer_exception,
          "error(x) whose toString threw raised a different exception\n");
    kt_not_implemented_reason(raising);
    CHECK(raised() == &kt_type_null_pointer_exception,
          "TODO(x) whose toString threw raised a different exception\n");

    kt_sys_write(1, "OK\n", 3);
}
