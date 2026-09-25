/* `Any.toString()` asks the object's own `hashCode` for the digits after the `@`, so a class that
   overrides `hashCode` and not `toString` reaches its override there -- and when that override
   THROWS, the exception propagates out of `toString` as Kotlin's does. The default rendering used
   to take the failed hash's placeholder and build the text from it anyway, handing back a string
   with an exception already in flight. */
#include "later_tiers.h"

static int hash_calls;

/* A `hashCode` that records a NullPointerException and comes back, as generated code does. */
static kt_int raising_hash_code(KRef self) {
    (void)self;
    hash_calls++;
    kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    return 0;
}

/* `kotlin.Any`'s `equals` and `toString`, and a `hashCode` of its own. */
static const kt_fn hashless_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)raising_hash_code,
                                        (kt_fn)kt_any_to_string};

static const KType hashless_type = {
    .name = "Hashless",
    .name_length = sizeof("Hashless") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = hashless_vtable,
    .vtable_length = 3,
};

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);

    KRef hashless = (KRef)kt_gc_allocate(&hashless_type, sizeof(KObjectHeader));
    KRef text = kt_to_string(hashless);
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    CHECK(hash_calls == 1, "the default toString did not ask the object's own hashCode\n");
    CHECK(thrown != NULL && type_of(thrown) == &kt_type_null_pointer_exception,
          "the default toString dropped the exception its hashCode threw\n");
    CHECK(text == NULL, "the default toString built a text after its hashCode threw\n");

    kt_sys_write(1, "OK\n", 3);
}
