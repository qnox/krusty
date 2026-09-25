/* A map or set whose element's `equals`, `hashCode` or `toString` THROWS stops where it threw, as
   Kotlin's does: the exception propagates out of the call, the collection is left as it was, and
   no further element is asked anything. `kt_throw` records and comes back, so each of these used to
   carry on -- `put` and `add` inserting the element whose comparison had just failed, and the
   builders, renderings and hashes calling into every element after it.

   A member that throws may answer anything, and the pending slot is the only thing that says it
   threw. So the hostile `equals` here can also answer TRUE after raising, which a caller that
   stops only on a false answer walks straight past: an entry compared its values after its keys'
   comparison threw, and a map's lookup overwrote, removed or matched the key the failed comparison
   pointed at. */
#include "later_tiers.h"

/* Whether the hostile element's members raise; off while a driver builds what it then checks. */
static kt_boolean armed;
static int equals_calls;
static int hash_calls;
static int to_string_calls;
/* Which call the armed `equals` raises from -- the first unless a case says otherwise, so a case
   can let a comparison succeed and have the NEXT one throw -- and what it answers once it has
   raised. */
static int equals_raises_from;
static kt_boolean equals_answer_after_raise;

static void raise(void) { kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL)); }

static kt_boolean hostile_equals(KRef self, KRef other) {
    equals_calls++;
    if (armed && equals_calls >= equals_raises_from) {
        raise();
        return equals_answer_after_raise;
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

/* Arm them with an `equals` that answers TRUE after it raises. */
static void arm_truthy(void) {
    arm();
    equals_answer_after_raise = 1;
}

/* The exception each case must end with; disarms for the next case. */
static void expect_raised(void) {
    CHECK(kt_pending_exception() != NULL, "a member threw and nothing propagated\n");
    kt_clear_pending();
    armed = 0;
    equals_raises_from = 1;
    equals_answer_after_raise = 0;
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
    equals_raises_from = 1;

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

    /* An entry whose VALUE's `toString` throws answers no text: the failed rendering is not joined
       in as though it were the value's. */
    KRef texted = kt_list_get(
        kt_map_keys_list(kt_map_entries(kt_map_of_pair(kt_pair_of(kt_string_utf8("k", 1), first)))),
        0);
    arm();
    KRef rendered = kt_to_string(texted);
    expect_raised();
    CHECK(rendered == NULL, "an entry rendered a text after its value's toString threw\n");

    /* A lookup whose comparison throws asks no key after it. */
    KRef two = kt_map_new();
    kt_map_set(two, first, text);
    kt_map_set(two, second, text);
    arm();
    (void)kt_map_get(two, third);
    expect_raised();
    CHECK(equals_calls == 1, "a lookup kept comparing keys after a comparison threw\n");

    arm();
    (void)kt_map_contains_value(valued, third);
    expect_raised();
    CHECK(equals_calls == 1, "containsValue kept comparing after a comparison threw\n");

    /* A comparison that throws and answers TRUE is no match: nothing is overwritten, removed or
       read through it. */
    KRef other = kt_string_utf8("w", 1);
    arm_truthy();
    (void)kt_map_put(two, first, other);
    expect_raised();
    CHECK(kt_map_get(two, first) == text, "`put` overwrote a value whose key comparison threw\n");

    arm_truthy();
    (void)kt_map_remove(two, first);
    expect_raised();
    CHECK(kt_map_size(two) == 2, "`remove` removed a key whose comparison threw\n");

    arm_truthy();
    (void)kt_set_remove(both, first);
    expect_raised();
    CHECK(kt_map_size(both) == 2, "a set removed an element whose comparison threw\n");

    arm_truthy();
    KRef found = kt_map_get(two, first);
    expect_raised();
    CHECK(found == NULL, "`get` answered the value of a key whose comparison threw\n");

    arm_truthy();
    found = kt_map_get_or_default(two, first, other);
    expect_raised();
    CHECK(found == NULL, "`getOrDefault` answered a value after its key comparison threw\n");

    /* Equality stops at a comparison that throws, whatever it answered. An entry compares its
       values only once its keys compared equal without raising. */
    KRef entry_to_third =
        kt_list_get(kt_map_keys_list(kt_map_entries(kt_map_of_pair(kt_pair_of(first, third)))), 0);
    arm_truthy();
    (void)kt_equals(entry, entry_to_third);
    expect_raised();
    CHECK(equals_calls == 1, "an entry compared its values after its keys' comparison threw\n");

    /* Two maps: a key search that throws and answers true asks no value. */
    KRef to_second = kt_map_of_pair(kt_pair_of(first, second));
    KRef to_third = kt_map_of_pair(kt_pair_of(first, third));
    arm_truthy();
    (void)kt_equals(to_second, to_third);
    expect_raised();
    CHECK(equals_calls == 1, "map equality compared a value after its key search threw\n");

    /* A key comparison that succeeds and a SECOND that would throw: the key is looked up once, so
       the only other comparison is the value's -- which is the one that throws. Looking the key up
       again compared the value with the NULL that failed lookup answered. */
    arm();
    equals_raises_from = 2;
    (void)kt_equals(to_second, to_third);
    expect_raised();
    CHECK(equals_calls == 2, "map equality looked a key up twice\n");

    /* A value comparison that throws and answers true ends the walk: no later entry is asked. */
    KRef values_a = kt_map_new();
    kt_map_set(values_a, kt_string_utf8("a", 1), first);
    kt_map_set(values_a, kt_string_utf8("b", 1), second);
    KRef values_b = kt_map_new();
    kt_map_set(values_b, kt_string_utf8("a", 1), first);
    kt_map_set(values_b, kt_string_utf8("b", 1), second);
    arm_truthy();
    (void)kt_equals(values_a, values_b);
    expect_raised();
    CHECK(equals_calls == 1, "map equality kept comparing after a value comparison threw\n");

    /* Two sets: an element search that throws and answers true looks for no later element. */
    KRef elements_a = kt_set_new();
    (void)kt_set_add(elements_a, first);
    (void)kt_set_add(elements_a, second);
    KRef elements_b = kt_set_new();
    (void)kt_set_add(elements_b, first);
    (void)kt_set_add(elements_b, second);
    arm_truthy();
    (void)kt_equals(elements_a, elements_b);
    expect_raised();
    CHECK(equals_calls == 1, "set equality kept looking after a comparison threw\n");

    kt_sys_write(1, "OK\n", 3);
}
