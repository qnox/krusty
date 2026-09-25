/* A `Pair` whose FIRST component's `equals`, `hashCode` or `toString` throws: the exception
   propagates from there, and the second component is never asked. When only the SECOND one throws,
   its placeholder is not read either: `equals` does not answer the TRUE it returned, and `hashCode`
   does not fold the placeholder into a hash. Each of the three members used
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
    /* Whether its members throw; one that does not answers as an ordinary object would. */
    kt_boolean throws;
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

static kt_boolean throws(KRef self) { return ((const Component *)self)->throws; }

static kt_boolean component_equals(KRef self, KRef other) {
    if (!throws(self)) {
        return self == other;
    }
    record_and_throw(self);
    return 1;
}

static kt_int component_hash_code(KRef self) {
    if (!throws(self)) {
        return 7;
    }
    record_and_throw(self);
    return 5;
}

static KRef component_to_string(KRef self) {
    if (!throws(self)) {
        return kt_string_utf8("a", 1);
    }
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

static KRef component(kt_int tag, kt_boolean throwing) {
    Component *made = (Component *)kt_gc_allocate(&component_type, sizeof(Component));
    made->tag = tag;
    made->throws = throwing;
    return (KRef)made;
}

/* After one member of the pair: exactly one call into the program threw, it was component `tag`'s,
   and what is pending is what that call threw. */
static void expect_stopped_at(kt_int tag) {
    CHECK(calls == 1, "a component was asked after one threw\n");
    CHECK(last_caller == tag, "the last call into the program was not the throwing component's\n");
    CHECK(kt_pending_exception() != NULL && kt_pending_exception() == last_thrown,
          "the throwing component's exception is not the one pending\n");
    kt_clear_pending();
    calls = 0;
    last_caller = 0;
    last_thrown = NULL;
}

void kt_program_entry(void) {
    KRef first = component(1, 1);
    KRef second = component(2, 1);
    KRef pair = kt_pair_of(first, second);
    /* A second pair of the same components, so `equals` gets past identity and asks them. */
    KRef same = kt_pair_of(first, second);

    (void)kt_equals(pair, same);
    expect_stopped_at(1);

    (void)kt_hash_code(pair);
    expect_stopped_at(1);

    KRef text = kt_to_string(pair);
    expect_stopped_at(1);
    CHECK(text == NULL, "a pair whose component threw still rendered text\n");

    /* Only the SECOND component throws. The first answers equal, a hash and text, and the second's
       placeholder must still not become the answer. */
    KRef calm = component(3, 0);
    KRef thrower = component(4, 1);
    KRef late = kt_pair_of(calm, thrower);
    KRef late_again = kt_pair_of(calm, thrower);

    CHECK(!kt_equals(late, late_again), "equals answered the second component's placeholder\n");
    expect_stopped_at(4);

    CHECK(kt_hash_code(late) == 0, "hashCode folded the second component's placeholder\n");
    expect_stopped_at(4);

    CHECK(kt_to_string(late) == NULL, "a pair whose second component threw rendered text\n");
    expect_stopped_at(4);

    kt_sys_write(1, "OK\n", 3);
}
