/* A map or set whose element's `equals`, `hashCode` or `toString` THROWS stops where it threw, as
   Kotlin's does: the exception propagates out of the call, the collection is left as it was, and
   no further element is asked anything. `kt_throw` records and comes back, so each of these used to
   carry on -- `put` and `add` inserting the element whose comparison had just failed, and the
   builders, renderings and hashes calling into every element after it. */
#include "later_tiers.h"

/* Whether the hostile element's members raise; off while a driver builds what it then checks. */
static kt_boolean armed;
static int equals_calls;
static int hash_calls;
static int to_string_calls;

static void raise(void) { kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL)); }

static kt_boolean hostile_equals(KRef self, KRef other) {
    equals_calls++;
    if (armed) {
        raise();
        return false;
    }
    return self == other;
}

static kt_int hostile_hash_code(KRef self) {
    hash_calls++;
    if (armed) {
        raise();
        return 0;
    }
    return kt_any_hash_code(self);
}

static KRef hostile_to_string(KRef self) {
    (void)self;
    to_string_calls++;
    if (armed) {
        raise();
        return NULL;
    }
    return kt_string_utf8("h", 1);
}

static const kt_fn hostile_vtable[] = {(kt_fn)hostile_equals, (kt_fn)hostile_hash_code,
                                       (kt_fn)hostile_to_string};

static const KType hostile_type = {
    .name = "Hostile",
    .name_length = sizeof("Hostile") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = hostile_vtable,
    .vtable_length = 3,
};

static KRef hostile(void) { return (KRef)kt_gc_allocate(&hostile_type, sizeof(KObjectHeader)); }

/* Arm the hostile members and zero their counters. */
static void arm(void) {
    armed = 1;
    equals_calls = 0;
    hash_calls = 0;
    to_string_calls = 0;
}

/* The exception each case must end with; disarms for the next case. */
static void expect_raised(void) {
    CHECK(kt_pending_exception() != NULL, "a member threw and nothing propagated\n");
    kt_clear_pending();
    armed = 0;
}

/* An `Array<Any?>` of three, as a vararg call packs one. */
static KRef array_of(KRef a, KRef b, KRef c) {
    KRef array = kt_array_new(&kt_type_array, 3);
    KRef *elements = (KRef *)((char *)array + kt_type_array.instance_size);
    elements[0] = a;
    elements[1] = b;
    elements[2] = c;
    return array;
}

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);

    KRef first = hostile();
    KRef second = hostile();
    KRef third = hostile();
    KRef text = kt_string_utf8("v", 1);

    /* `put` and `add` whose search threw insert nothing. */
    KRef map = kt_map_new();
    kt_map_set(map, first, text);
    arm();
    (void)kt_map_put(map, second, text);
    expect_raised();
    CHECK(kt_map_size(map) == 1, "`put` inserted a key whose comparison threw\n");

    KRef set = kt_set_new();
    (void)kt_set_add(set, first);
    arm();
    (void)kt_set_add(set, second);
    expect_raised();
    CHECK(kt_map_size(set) == 1, "`add` inserted an element whose comparison threw\n");

    /* `setOf` and `mapOf` stop at the element that threw. */
    arm();
    (void)kt_set_of(array_of(first, second, third));
    expect_raised();
    CHECK(equals_calls == 1, "`setOf` kept comparing after a comparison threw\n");

    arm();
    (void)kt_map_of(array_of(kt_pair_of(first, text), kt_pair_of(second, text),
                             kt_pair_of(third, text)));
    expect_raised();
    CHECK(equals_calls == 1, "`mapOf` kept comparing after a comparison threw\n");

    /* Rendering and hashing stop at the element that threw. */
    KRef valued = kt_map_new();
    kt_map_set(valued, kt_string_utf8("a", 1), first);
    kt_map_set(valued, kt_string_utf8("b", 1), second);
    KRef both = kt_set_new();
    (void)kt_set_add(both, first);
    (void)kt_set_add(both, second);
    KRef pair = kt_map_of_pair(kt_pair_of(first, second));
    KRef entry = kt_list_get(kt_map_keys_list(kt_map_entries(pair)), 0);

    arm();
    (void)kt_to_string(valued);
    expect_raised();
    CHECK(to_string_calls == 1, "a map kept rendering after a value's toString threw\n");

    arm();
    (void)kt_to_string(both);
    expect_raised();
    CHECK(to_string_calls == 1, "a set kept rendering after an element's toString threw\n");

    arm();
    (void)kt_to_string(entry);
    expect_raised();
    CHECK(to_string_calls == 1, "an entry rendered its value after its key's toString threw\n");

    arm();
    (void)kt_hash_code(valued);
    expect_raised();
    CHECK(hash_calls == 1, "a map kept hashing after a value's hashCode threw\n");

    /* A key that throws ends the hash before its value is asked. */
    KRef keyed = kt_map_new();
    kt_map_set(keyed, first, second);
    arm();
    (void)kt_hash_code(keyed);
    expect_raised();
    CHECK(hash_calls == 1, "a map hashed a value after its key's hashCode threw\n");

    arm();
    (void)kt_hash_code(both);
    expect_raised();
    CHECK(hash_calls == 1, "a set kept hashing after an element's hashCode threw\n");

    arm();
    (void)kt_hash_code(entry);
    expect_raised();
    CHECK(hash_calls == 1, "an entry hashed its value after its key's hashCode threw\n");

    kt_sys_write(1, "OK\n", 3);
}
