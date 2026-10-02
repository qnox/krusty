/* A call through a dispatched member follows `kt_dispatch`'s contract, even on a `null` receiver.

   The sequence a call site emits (`krusty_rt.h`): dispatch; then check the pending slot, and when an
   exception is pending, propagate it and make no call; only then call the member at the slot's own
   signature, the receiver first. On a `null` receiver `kt_dispatch` raises `NullPointerException`
   with no message and answers NULL, so the check stops the site before the call, and nothing is
   ever called through NULL or through a function type other than the member's own, which C leaves
   undefined. A non-null receiver goes through the same sequence to its member and gets its answer.

   `Counter` stands for a class of the program with two members past `kotlin.Any`'s three, each
   taking arguments and answering a value: `plus(Int): Int` and `pick(Any?): Any?`. Each counts its
   calls, so the driver sees that a `null` receiver reaches neither. Built with the harness's
   flags, signed-overflow traps included; the driver prints nothing but its `OK`. */
#include "driver_exceptions.h"

enum { SLOT_PLUS = 3, SLOT_PICK = 4 };

typedef struct Counter {
    KObjectHeader header;
    kt_int base;
} Counter;

static int plus_calls;
static int pick_calls;

static kt_int counter_plus(KRef self, kt_int amount) {
    plus_calls++;
    return ((const Counter *)self)->base + amount;
}

static KRef counter_pick(KRef self, KRef other) {
    pick_calls++;
    return other != NULL ? other : self;
}

static const kt_fn counter_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                       (kt_fn)kt_any_to_string, (kt_fn)counter_plus,
                                       (kt_fn)counter_pick};

static const KType counter_type = {
    .name = "Counter",
    .name_length = 7,
    .instance_size = sizeof(Counter),
    .super = &kt_type_any,
    .vtable = counter_vtable,
    .vtable_length = 5,
};

/* What a call site emits for `receiver.plus(amount)`: 0 stands for the answer nobody reads once an
   exception is pending, which the caller propagates. */
static kt_int call_plus(KRef receiver, kt_int amount) {
    kt_fn member = kt_dispatch(receiver, SLOT_PLUS);
    if (kt_pending_exception() != NULL) {
        return 0;
    }
    return ((kt_int(*)(KRef, kt_int))member)(receiver, amount);
}

/* And for `receiver.pick(other)`. */
static KRef call_pick(KRef receiver, KRef other) {
    kt_fn member = kt_dispatch(receiver, SLOT_PICK);
    if (kt_pending_exception() != NULL) {
        return NULL;
    }
    return ((KRef(*)(KRef, KRef))member)(receiver, other);
}

static kt_boolean raised_npe(void) {
    return took_message(&kt_type_null_pointer_exception, NULL);
}

void kt_program_entry(void) {
    DRIVER_BEGIN();

    CHECK(kt_dispatch(NULL, SLOT_PLUS) == NULL && raised_npe(),
          "dispatch on null did not raise NullPointerException and answer NULL\n");

    CHECK(call_plus(NULL, 5) == 0 && raised_npe() && plus_calls == 0,
          "plus on null called its member or raised no NullPointerException\n");
    CHECK(call_pick(NULL, kt_box_int(1)) == NULL && raised_npe() && pick_calls == 0,
          "pick on null called its member or raised no NullPointerException\n");

    Counter *counter = (Counter *)kt_gc_allocate(&counter_type, sizeof(Counter));
    counter->base = 40;
    KRef receiver = (KRef)counter;
    CHECK(call_plus(receiver, 2) == 42 && kt_pending_exception() == NULL && plus_calls == 1,
          "plus on a receiver did not answer its member's value\n");
    KRef other = kt_box_int(7);
    CHECK(call_pick(receiver, other) == other && call_pick(receiver, NULL) == receiver &&
              kt_pending_exception() == NULL && pick_calls == 2,
          "pick on a receiver did not answer its member's value\n");

    /* A null receiver after a call that went through changes nothing that came before. */
    CHECK(call_plus(NULL, 1) == 0 && raised_npe() && plus_calls == 1 && counter->base == 40,
          "plus on null after a call reached the member\n");

    kt_sys_write(1, "OK\n", 3);
}
