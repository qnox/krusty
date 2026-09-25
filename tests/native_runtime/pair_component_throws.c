/* A `Pair` whose FIRST component's `equals`, `hashCode` or `toString` throws: the exception
   propagates from there, and the second component is never asked. Each of the three members used
   to ask both components in one expression, so the second component's override ran after the first
   had already thrown -- program code Kotlin never reaches -- and `toString` went on to build text
   from the placeholder the aborted call returned.

   Every component here throws from all three members and answers with a placeholder a caller must
   not read: `equals` a TRUE, so that reading it would carry on to the second component exactly as a
   genuine match would. Each call records which component it was and the exception it threw, which
   is how the driver proves the first component's call was the LAST call into the program and that
   the exception pending afterwards is the very one it threw. */
#include "later_tiers.h"

typedef struct Component {
    KObjectHeader header;
    kt_int tag;
} Component;

static int calls;
static kt_int last_caller;
static KRef last_thrown;

static void record_and_throw(KRef self) {
    calls++;
    last_caller = ((const Component *)self)->tag;
    last_thrown = kt_throwable_new(&kt_type_null_pointer_exception, NULL);
    kt_throw(last_thrown);
}

static kt_boolean component_equals(KRef self, KRef other) {
    (void)other;
    record_and_throw(self);
    return 1;
}

static kt_int component_hash_code(KRef self) {
    record_and_throw(self);
    return 0;
}

static KRef component_to_string(KRef self) {
    record_and_throw(self);
    return NULL;
}

static const kt_fn component_vtable[] = {(kt_fn)component_equals, (kt_fn)component_hash_code,
                                         (kt_fn)component_to_string};

static const KType component_type = {
    .name = "Component",
    .name_length = sizeof("Component") - 1,
    .instance_size = sizeof(Component),
    .super = &kt_type_any,
    .vtable = component_vtable,
    .vtable_length = 3,
};

static KRef component(kt_int tag) {
    Component *made = (Component *)kt_gc_allocate(&component_type, sizeof(Component));
    made->tag = tag;
    return (KRef)made;
}

/* After one member of the pair: exactly one call reached the program, it was the first component's,
   and what is pending is what that call threw. */
static void expect_stopped_at_first(void) {
    CHECK(calls == 1, "the second component was asked after the first one threw\n");
    CHECK(last_caller == 1, "the last call into the program was not the first component's\n");
    CHECK(kt_pending_exception() != NULL && kt_pending_exception() == last_thrown,
          "the first component's exception is not the one pending\n");
    kt_clear_pending();
    calls = 0;
    last_caller = 0;
    last_thrown = NULL;
}

void kt_program_entry(void) {
    KRef first = component(1);
    KRef second = component(2);
    KRef pair = kt_pair_of(first, second);
    /* A second pair of the same components, so `equals` gets past identity and asks them. */
    KRef same = kt_pair_of(first, second);

    (void)kt_equals(pair, same);
    expect_stopped_at_first();

    (void)kt_hash_code(pair);
    expect_stopped_at_first();

    KRef text = kt_to_string(pair);
    expect_stopped_at_first();
    CHECK(text == NULL, "a pair whose component threw still rendered text\n");

    kt_sys_write(1, "OK\n", 3);
}
