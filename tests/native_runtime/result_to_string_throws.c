/* `Result.toString()` over a value, or an exception, whose own `toString` throws: the rendering
   stops there and answers no text, with that exception still the one pending. Both branches used to
   splice whatever the aborted `toString` returned into `Success(...)` or `Failure(...)`, allocating
   and handing back text after the call it was built from had already failed.

   The object's `toString` throws a fresh exception and returns a NULL placeholder, which the
   rendering would otherwise have printed as `null`. */
#include "later_tiers.h"

static int calls;
static KRef last_thrown;

static KRef throwing_to_string(KRef self) {
    (void)self;
    calls++;
    last_thrown = kt_throwable_new(&kt_type_null_pointer_exception, NULL);
    kt_throw(last_thrown);
    return NULL;
}

/* `equals` and `hashCode` are never asked here; they fail loudly if they are. */
static kt_boolean unexpected_equals(KRef self, KRef other) {
    (void)self;
    (void)other;
    KT_SYS_FAIL("the rendering asked for equals\n");
    return 0;
}

static kt_int unexpected_hash_code(KRef self) {
    (void)self;
    KT_SYS_FAIL("the rendering asked for hashCode\n");
    return 0;
}

static const kt_fn throwing_vtable[] = {(kt_fn)unexpected_equals, (kt_fn)unexpected_hash_code,
                                        (kt_fn)throwing_to_string};

static const KType throwing_type = {
    .name = "Throwing",
    .name_length = sizeof("Throwing") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = throwing_vtable,
    .vtable_length = 3,
};

/* After one rendering: no text, one call into the program, and that call's exception pending. */
static void expect_no_text(KRef rendered) {
    CHECK(rendered == NULL, "a Result whose content threw still rendered text\n");
    CHECK(calls == 1, "the content's toString was not asked exactly once\n");
    CHECK(kt_pending_exception() != NULL && kt_pending_exception() == last_thrown,
          "the content's exception is not the one pending\n");
    kt_clear_pending();
    calls = 0;
    last_thrown = NULL;
}

void kt_program_entry(void) {
    KRef throwing = (KRef)kt_gc_allocate(&throwing_type, sizeof(KObjectHeader));

    /* A success renders its value, a failure the exception it holds: the same object both times. */
    expect_no_text(kt_result_to_string(kt_result_success(throwing)));
    expect_no_text(kt_result_to_string(kt_result_failure(throwing)));

    kt_sys_write(1, "OK\n", 3);
}
