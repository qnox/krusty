/* A `lazy` whose initializer throws is NOT initialized: the exception propagates and the next read
   runs the initializer again, as Kotlin's does. The lazy used to mark itself computed whatever the
   initializer did, so a first read that threw left behind a value the aborted frame never produced
   -- a null every later read handed back without a word. */
#include "later_tiers.h"

static int invocations;
/* What the first `invoke` threw, so the exception pending afterwards is checked by identity. */
static KRef thrown;

/* `Function0`'s `invoke`: throws the first time, answers a boxed 42 after that. */
static KRef initializer_invoke(KRef self) {
    (void)self;
    invocations++;
    if (invocations == 1) {
        thrown = kt_throwable_new(&kt_type_null_pointer_exception, NULL);
        kt_throw(thrown);
        return NULL;
    }
    return kt_box_int(42);
}

static const kt_fn initializer_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                           (kt_fn)kt_any_to_string, (kt_fn)initializer_invoke};

static const KType initializer_type = {
    .name = "LazyInitializer",
    .name_length = sizeof("LazyInitializer") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = initializer_vtable,
    .vtable_length = 4,
};

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef lazy = kt_lazy_of((KRef)kt_gc_allocate(&initializer_type, sizeof(KObjectHeader)));

    (void)kt_lazy_value(lazy);
    CHECK(thrown != NULL && kt_pending_exception() == thrown,
          "the initializer's exception is not the one pending\n");
    CHECK(!kt_lazy_is_initialized(lazy), "a lazy whose initializer threw counts as initialized\n");
    kt_clear_pending();

    KRef value = kt_lazy_value(lazy);
    CHECK(invocations == 2, "the second read did not run the initializer again\n");
    CHECK(kt_pending_exception() == NULL, "the second read raised\n");
    CHECK(value != NULL && kt_unbox_int(value) == 42, "the second read lost the value\n");
    CHECK(kt_lazy_is_initialized(lazy), "a lazy that computed its value is not initialized\n");

    CHECK(kt_lazy_value(lazy) == value && invocations == 2, "the value was not kept\n");
    kt_sys_write(1, "OK\n", 3);
}
