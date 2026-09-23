/* `m.keys` and `m.entries` copy what the map holds without comparing any of it: a map's keys are
   distinct already, so there is nothing to check. Both used to insert each one through `add`,
   which searched everything inserted before it by `equals` -- a quadratic walk, paid again on
   every `for ((k, v) in m)`, since iterating a map iterates its entries. */
#include "later_tiers.h"

#define KEYS 64

static int comparisons;

/* A key that counts the times it is compared, and is otherwise equal only to itself. */
static kt_boolean counting_equals(KRef self, KRef other) {
    comparisons++;
    return self == other;
}

static const kt_fn counting_vtable[] = {(kt_fn)counting_equals, (kt_fn)kt_any_hash_code,
                                        (kt_fn)kt_any_to_string};

static const KType counting_type = {
    .name = "CountingKey",
    .name_length = sizeof("CountingKey") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = counting_vtable,
    .vtable_length = 3,
};

static KRef keys[KEYS];
static KRef values[KEYS];

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);

    KRef map = kt_map_new();
    for (int at = 0; at < KEYS; at++) {
        /* Static storage is no root of its own; each slot is registered before it is filled. */
        kt_gc_add_global_root((void **)&keys[at]);
        kt_gc_add_global_root((void **)&values[at]);
        keys[at] = (KRef)kt_gc_allocate(&counting_type, sizeof(KObjectHeader));
        values[at] = kt_string_utf8("v", 1);
        kt_map_set(map, keys[at], values[at]);
    }

    comparisons = 0;
    KRef key_set = kt_map_keys(map);
    CHECK(comparisons == 0, "`keys` compared the keys it copied\n");
    CHECK(kt_map_size(key_set) == KEYS, "`keys` lost a key\n");
    KRef listed = kt_map_keys_list(key_set);
    for (int at = 0; at < KEYS; at++) {
        CHECK(kt_list_get(listed, at) == keys[at], "`keys` is out of insertion order\n");
    }

    comparisons = 0;
    KRef entries = kt_map_entries(map);
    CHECK(comparisons == 0, "`entries` compared the keys it copied\n");
    CHECK(kt_map_size(entries) == KEYS, "`entries` lost an entry\n");
    listed = kt_map_keys_list(entries);
    for (int at = 0; at < KEYS; at++) {
        KRef entry = kt_list_get(listed, at);
        CHECK(kt_map_entry_key(entry) == keys[at] && kt_map_entry_value(entry) == values[at],
              "`entries` is out of insertion order\n");
    }

    CHECK(kt_pending_exception() == NULL, "a view raised\n");
    kt_sys_write(1, "OK\n", 3);
}
