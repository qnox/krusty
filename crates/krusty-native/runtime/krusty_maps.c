/* krusty native runtime: maps and sets, and the companions and unit the runtime keeps as static
   objects.

   A translation unit of its own, beside the lists of `krusty_collections.c`, so that no file of the
   runtime grows past what one file should hold. It shares the object and list layouts through
   `krusty_internal.h`. */
#include "krusty_internal.h"

/* ---- maps and sets --------------------------------------------------------------------------

   Kotlin/Native's two classes: `HashMap`, which `LinkedHashMap` is a type alias for, and `HashSet`,
   a set backed by a `HashMap`, which `LinkedHashSet` is a type alias for. The runtime behaves as
   Kotlin/Native's stdlib does, statement for statement (`HashMap.kt` and `HashSet.kt`, JetBrains/
   kotlin v2.4.10, kotlin-native/runtime/src/main/kotlin/kotlin/collections and
   libraries/stdlib/native-wasm/src/kotlin/collections), and speaks as the JVM does where that is
   cheap -- an exception's message:

   - the keys, and a map's values, in arrays in insertion order, a removed key leaving a hole that
     a later growth compacts away, so every map iterates in insertion order;
   - an open-addressed hash array of slots holding a key's index plus one, probed downward from the
     key's hash, `(hashCode * 0x9E3779B9) ushr shift`, for at most the longest probe any key needed;
     a probe that would go too far doubles the hash array, and growing the arrays rehashes every
     key -- asking each its `hashCode` again;
   - a lookup that asks the STORED key's `equals` of the key looked for, `stored == key`, with no
     identity check and no comparison of hashes first;
   - iterators that skip holes, fail with `ConcurrentModificationException` at `next()` once the
     map has changed structurally, and hand out a fresh `EntryRef` per entry, which reads the map
     through its index;
   - `keys`, `values` and `entries` as live views, the same object every time.

   `mapOf` and `setOf` with nothing to hold answer neither class but the stdlib's shared read-only
   `EmptyMap` and `EmptySet` (below). */

typedef struct KMap {
    KObjectHeader header;
    /* `keysArray`: the keys in insertion order, a removed one's slot NULL. */
    KRef keys;
    /* `valuesArray`: the values by the same index, made by the first `put`; NULL in a set. */
    KRef values;
    /* `presenceArray`: each index's slot in the hash array, -1 once removed. */
    KRef presence;
    /* `hashArray`: each slot's index plus one, 0 for an empty slot. */
    KRef hashes;
    /* The three views, each made on first use and kept, so `m.keys === m.keys`. */
    KRef keys_view;
    KRef values_view;
    KRef entries_view;
    /* The longest probe a key has needed. */
    kt_int max_probe_distance;
    /* The next index a new key takes: the size plus the holes. */
    kt_int length;
    /* How far a scrambled hash is shifted down to index the hash array. */
    kt_int hash_shift;
    kt_int size;
    /* Structural changes so far, `modCount`, on the unsigned ring. */
    uint32_t modifications;
} KMap;

/* A view: the map it shows. */
typedef struct KMapView {
    KObjectHeader header;
    KRef map;
} KMapView;

/* `HashMap.Itr`: `index` is the next index `next()` hands out, `last` the one it handed out last,
   `expected` the modification count the iterator was made at or last removed at. The runtime's own
   walks keep one in a local. */
typedef struct KMapIterator {
    KObjectHeader header;
    KRef map;
    kt_int index;
    kt_int last;
    uint32_t expected;
} KMapIterator;

/* `HashMap.EntryRef`: a map and an index, and the modification count it was made at. */
typedef struct KMapEntry {
    KObjectHeader header;
    KRef map;
    kt_int index;
    uint32_t expected;
} KMapEntry;

static const uint32_t kt_map_offsets[] = {
    offsetof(KMap, keys),      offsetof(KMap, values),      offsetof(KMap, presence),
    offsetof(KMap, hashes),    offsetof(KMap, keys_view),   offsetof(KMap, values_view),
    offsetof(KMap, entries_view)};
static const uint32_t kt_map_view_offsets[] = {offsetof(KMapView, map)};
static const uint32_t kt_map_iterator_offsets[] = {offsetof(KMapIterator, map)};
static const uint32_t kt_map_entry_offsets[] = {offsetof(KMapEntry, map)};

static kt_boolean kt_map_equals(KRef self, KRef other);
static kt_int kt_map_hash_code(KRef self);
static KRef kt_map_to_string(KRef self);
static kt_boolean kt_set_equals(KRef self, KRef other);
static kt_int kt_set_hash_code(KRef self);
static KRef kt_set_to_string(KRef self);
static kt_boolean kt_entry_equals(KRef self, KRef other);
static kt_int kt_entry_hash_code(KRef self);
static KRef kt_entry_to_string(KRef self);
static KRef kt_map_walk(KRef self);
static kt_boolean kt_map_iterator_has_next(KRef self);
static KRef kt_map_iterator_next(KRef self);

static const kt_fn kt_map_vtable[] = {(kt_fn)kt_map_equals, (kt_fn)kt_map_hash_code,
                                      (kt_fn)kt_map_to_string};
static const kt_fn kt_set_vtable[] = {(kt_fn)kt_set_equals, (kt_fn)kt_set_hash_code,
                                      (kt_fn)kt_set_to_string};
static const kt_fn kt_entry_vtable[] = {(kt_fn)kt_entry_equals, (kt_fn)kt_entry_hash_code,
                                        (kt_fn)kt_entry_to_string};
/* `values` is an `AbstractMutableCollection`, which overrides neither `equals` nor `hashCode`: it
   is equal only to itself. */
static const kt_fn kt_values_view_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                              (kt_fn)kt_set_to_string};
/* ---- descriptors ---------------------------------------------------------------------------- */

/* The map, set and entry interfaces, as an `is` names them, each listing its own bases,
   flattened. */
static const KType *const kt_mutable_map_bases[] = {&kt_type_map_interface};
static const KType *const kt_set_bases[] = {&kt_type_collection_interface,
                                            &kt_type_iterable_interface};
static const KType *const kt_mutable_set_bases[] = {
    &kt_type_set_interface, &kt_type_mutable_collection_interface, &kt_type_collection_interface,
    &kt_type_mutable_iterable_interface, &kt_type_iterable_interface};
static const KType *const kt_mutable_entry_bases[] = {&kt_type_map_entry};

#define KT_INTERFACE(identifier, package, simple, bases, count)                                    \
    const KType identifier = {KT_NAMED(package, simple),                                           \
                              .instance_size = sizeof(KObjectHeader),                              \
                              .super = &kt_type_any,                                               \
                              .vtable = kt_any_vtable,                                             \
                              .vtable_length = 3,                                                  \
                              .interfaces = bases,                                                 \
                              .interface_count = count};

KT_INTERFACE(kt_type_map_interface, "kotlin.collections.", "Map", NULL, 0)
KT_INTERFACE(kt_type_mutable_map_interface, "kotlin.collections.", "MutableMap",
             kt_mutable_map_bases, 1)
KT_INTERFACE(kt_type_set_interface, "kotlin.collections.", "Set", kt_set_bases, 2)
KT_INTERFACE(kt_type_mutable_set_interface, "kotlin.collections.", "MutableSet",
             kt_mutable_set_bases, 5)
KT_INTERFACE(kt_type_map_entry, "kotlin.collections.Map.", "Entry", NULL, 0)
KT_INTERFACE(kt_type_mutable_map_entry, "kotlin.collections.MutableMap.", "MutableEntry",
             kt_mutable_entry_bases, 1)

#undef KT_INTERFACE

/* What each class implements, transitively. */
static const KType *const kt_map_interfaces[] = {&kt_type_mutable_map_interface,
                                                 &kt_type_map_interface};
static const KType *const kt_set_interfaces[] = {
    &kt_type_mutable_set_interface,        &kt_type_set_interface,
    &kt_type_mutable_collection_interface, &kt_type_collection_interface,
    &kt_type_mutable_iterable_interface,   &kt_type_iterable_interface};
static const KType *const kt_collection_interfaces[] = {
    &kt_type_mutable_collection_interface, &kt_type_collection_interface,
    &kt_type_mutable_iterable_interface, &kt_type_iterable_interface};
static const KType *const kt_entry_interfaces[] = {&kt_type_mutable_map_entry,
                                                   &kt_type_map_entry};
static const KType *const kt_iterator_interfaces[] = {&kt_type_iterator_interface};

/* The abstract classes a set and the views extend, which a program sees only as a superclass. */
#define KT_ABSTRACT(identifier, package, simple, parent)                                           \
    static const KType identifier = {KT_NAMED(package, simple),                                    \
                                     .instance_size = sizeof(KObjectHeader),                       \
                                     .super = parent,                                              \
                                     .vtable = kt_any_vtable,                                      \
                                     .vtable_length = 3};
KT_ABSTRACT(kt_type_abstract_collection, "kotlin.collections.", "AbstractCollection", &kt_type_any)
KT_ABSTRACT(kt_type_abstract_mutable_collection, "kotlin.collections.",
            "AbstractMutableCollection", &kt_type_abstract_collection)
KT_ABSTRACT(kt_type_abstract_mutable_set, "kotlin.collections.", "AbstractMutableSet",
            &kt_type_abstract_mutable_collection)
KT_ABSTRACT(kt_type_hash_map_entry_set_base, "kotlin.collections.", "HashMapEntrySetBase",
            &kt_type_abstract_mutable_set)
/* `HashMap.Itr`, the open class the three iterators extend. */
KT_ABSTRACT(kt_type_hash_map_itr, "kotlin.collections.HashMap.", "Itr", &kt_type_any)
#undef KT_ABSTRACT

/* The classes are Kotlin/Native's own, `kotlin.collections.*`, which is what `::class` names there.
   `storage` is empty for a class the header publishes and `static` for one only this file
   reaches. */
#define KT_MAP_CLASS(storage, identifier, package, simple, shape, offsets, parent, table,          \
                     implemented, walk)                                                            \
    storage const KType identifier = {                                                             \
        KT_NAMED(package, simple),                                                                 \
        .instance_size = sizeof(shape),                                                            \
        .reference_count = sizeof(offsets) / sizeof(uint32_t),                                     \
        .reference_offsets = offsets,                                                              \
        .super = parent,                                                                           \
        .vtable = table,                                                                           \
        .vtable_length = 3,                                                                        \
        .interfaces = implemented,                                                                 \
        .interface_count = sizeof(implemented) / sizeof(KType *),                                  \
        .walk_iterator = walk};

KT_MAP_CLASS(, kt_type_hash_map, "kotlin.collections.", "HashMap", KMap, kt_map_offsets,
             &kt_type_any, kt_map_vtable, kt_map_interfaces, kt_map_walk)
KT_MAP_CLASS(, kt_type_hash_set, "kotlin.collections.", "HashSet", KMap, kt_map_offsets,
             &kt_type_abstract_mutable_set, kt_set_vtable, kt_set_interfaces, kt_map_walk)
KT_MAP_CLASS(static, kt_type_map_entry_ref, "kotlin.collections.HashMap.", "EntryRef", KMapEntry,
             kt_map_entry_offsets, &kt_type_any, kt_entry_vtable, kt_entry_interfaces, NULL)
KT_MAP_CLASS(static, kt_type_keys_view, "kotlin.collections.", "HashMapKeys", KMapView,
             kt_map_view_offsets, &kt_type_abstract_mutable_set, kt_set_vtable,
             kt_set_interfaces, kt_map_walk)
KT_MAP_CLASS(static, kt_type_values_view, "kotlin.collections.", "HashMapValues", KMapView,
             kt_map_view_offsets, &kt_type_abstract_mutable_collection, kt_values_view_vtable,
             kt_collection_interfaces, kt_map_walk)
KT_MAP_CLASS(static, kt_type_entries_view, "kotlin.collections.", "HashMapEntrySet", KMapView,
             kt_map_view_offsets, &kt_type_hash_map_entry_set_base, kt_set_vtable,
             kt_set_interfaces, kt_map_walk)

#undef KT_MAP_CLASS

/* The iterators, Kotlin/Native's three: `HashMap.KeysItr`, `ValuesItr` and `EntriesItr`, which
   extend `HashMap.Itr`. A set's is its map's key iterator, and a map's own `iterator()` -- the
   `Map.iterator()` extension -- its entries'. */
#define KT_MAP_ITERATOR(identifier, simple)                                                        \
    static const KType identifier = {                                                              \
        KT_NAMED("kotlin.collections.HashMap.", simple),                                           \
        .instance_size = sizeof(KMapIterator),                                                     \
        .reference_count = sizeof(kt_map_iterator_offsets) / sizeof(uint32_t),                     \
        .reference_offsets = kt_map_iterator_offsets,                                              \
        .super = &kt_type_hash_map_itr,                                                            \
        .vtable = kt_any_vtable,                                                                   \
        .vtable_length = 3,                                                                        \
        .interfaces = kt_iterator_interfaces,                                                      \
        .interface_count = 1,                                                                      \
        .walk_has_next = kt_map_iterator_has_next,                                                 \
        .walk_next = kt_map_iterator_next};
KT_MAP_ITERATOR(kt_type_keys_iterator, "KeysItr")
KT_MAP_ITERATOR(kt_type_values_iterator, "ValuesItr")
KT_MAP_ITERATOR(kt_type_entries_iterator, "EntriesItr")
#undef KT_MAP_ITERATOR

/* ---- the empty map and set ------------------------------------------------------------------

   What `emptyMap()` and `emptySet()` answer, and so `mapOf()` and `setOf()`, and `mapOf(*pairs)`
   and `setOf(*elements)` of an empty array: the stdlib's common `EmptyMap`, `EmptySet` and
   `EmptyIterator` objects (libraries/stdlib/src/kotlin/collections/Maps.kt, Sets.kt and
   Collections.kt, JetBrains/kotlin v2.4.10), which Kotlin/Native compiles as they are written.
   Each is one object for the program's life, in static storage as `kt_unit` is, so every call
   answers the same one. They are read-only: `EmptyMap` is a `Map` and no `MutableMap`, and
   `EmptySet` a `Set` and no `MutableSet`, so a cast to the mutable interface fails before any
   member could write. `equals` is `other is Map<*, *> && other.isEmpty()` (`Set<*>` for the set),
   `hashCode` 0 and `toString` `{}` and `[]`; a lookup answers absent without calling the key;
   `keys` and `entries` are `EmptySet`; and every iterator of either is `EmptyIterator`, whose
   `hasNext` is false and whose `next` raises `NoSuchElementException`. `EmptyMap.values` is the
   stdlib's `EmptyList`, which this runtime does not have: it answers its own empty list there, as
   it answers `emptyList()`. `hashMapOf` and `hashSetOf` are always a fresh `HashMap` or `HashSet`,
   however little they hold. Which object a member was handed is its descriptor's answer. */
static kt_boolean kt_empty_map_equals(KRef self, KRef other);
static kt_boolean kt_empty_set_equals(KRef self, KRef other);
static kt_int kt_empty_hash_code(KRef self);
static KRef kt_empty_map_to_string(KRef self);
static KRef kt_empty_set_to_string(KRef self);
static KRef kt_empty_walk(KRef self);
static kt_boolean kt_empty_iterator_has_next(KRef self);
static KRef kt_empty_iterator_next(KRef self);

static const kt_fn kt_empty_map_vtable[] = {(kt_fn)kt_empty_map_equals, (kt_fn)kt_empty_hash_code,
                                            (kt_fn)kt_empty_map_to_string};
static const kt_fn kt_empty_set_vtable[] = {(kt_fn)kt_empty_set_equals, (kt_fn)kt_empty_hash_code,
                                            (kt_fn)kt_empty_set_to_string};
static const KType *const kt_empty_map_interfaces[] = {&kt_type_map_interface};
static const KType *const kt_empty_set_interfaces[] = {
    &kt_type_set_interface, &kt_type_collection_interface, &kt_type_iterable_interface};

static const KType kt_type_empty_map = {
    KT_NAMED("kotlin.collections.", "EmptyMap"),
    .instance_size = sizeof(KObject),
    .super = &kt_type_any,
    .vtable = kt_empty_map_vtable,
    .vtable_length = 3,
    .interfaces = kt_empty_map_interfaces,
    .interface_count = sizeof(kt_empty_map_interfaces) / sizeof(KType *),
    .walk_iterator = kt_empty_walk};
static const KType kt_type_empty_set = {
    KT_NAMED("kotlin.collections.", "EmptySet"),
    .instance_size = sizeof(KObject),
    .super = &kt_type_any,
    .vtable = kt_empty_set_vtable,
    .vtable_length = 3,
    .interfaces = kt_empty_set_interfaces,
    .interface_count = sizeof(kt_empty_set_interfaces) / sizeof(KType *),
    .walk_iterator = kt_empty_walk};
static const KType kt_type_empty_iterator = {KT_NAMED("kotlin.collections.", "EmptyIterator"),
                                             .instance_size = sizeof(KObject),
                                             .super = &kt_type_any,
                                             .vtable = kt_any_vtable,
                                             .vtable_length = 3,
                                             .interfaces = kt_iterator_interfaces,
                                             .interface_count = 1,
                                             .walk_has_next = kt_empty_iterator_has_next,
                                             .walk_next = kt_empty_iterator_next};

static KObject kt_the_empty_map = {{&kt_type_empty_map}, {{NULL, NULL, 0}}};
static KObject kt_the_empty_set = {{&kt_type_empty_set}, {{NULL, NULL, 0}}};
static KObject kt_the_empty_iterator = {{&kt_type_empty_iterator}, {{NULL, NULL, 0}}};

KRef kt_map_empty(void) { return &kt_the_empty_map; }
KRef kt_set_empty(void) { return &kt_the_empty_set; }

/* Whether `value` is `EmptyMap` or `EmptySet`, which hold nothing and have no table. */
static kt_boolean kt_is_empty_one(KRef value) {
    return value != NULL &&
           (value->header.type == &kt_type_empty_map || value->header.type == &kt_type_empty_set);
}

static kt_boolean kt_is_view_type(const KType *type) {
    return type == &kt_type_keys_view || type == &kt_type_values_view ||
           type == &kt_type_entries_view;
}

kt_boolean kt_is_map(KRef value) {
    return value != NULL && (value->header.type == &kt_type_hash_map ||
                             value->header.type == &kt_type_empty_map);
}

/* A set, `EmptySet`, or a map's `keys` or `entries`, which are sets too. */
kt_boolean kt_is_set(KRef value) {
    if (value == NULL) {
        return false;
    }
    const KType *type = value->header.type;
    return type == &kt_type_hash_set || type == &kt_type_empty_set || type == &kt_type_keys_view ||
           type == &kt_type_entries_view;
}

/* The map behind a map, a set or a view; NULL for anything else. */
static KMap *kt_map_behind(KRef value) {
    if (value == NULL) {
        return NULL;
    }
    const KType *type = value->header.type;
    if (type == &kt_type_hash_map || type == &kt_type_hash_set) {
        return (KMap *)value;
    }
    if (kt_is_view_type(type)) {
        return (KMap *)((const KMapView *)value)->map;
    }
    return NULL;
}

/* The receiver's map. A receiver that has none is a routing mistake, and fails loudly. */
static KMap *kt_map_of_receiver(KRef value) {
    KMap *map = kt_map_behind(value);
    if (map == NULL && kt_is_empty_one(value)) {
        KT_FAIL("krusty: a mutable map or set member on the read-only EmptyMap or EmptySet\n");
    }
    if (map == NULL) {
        KT_FAIL("krusty: a map or set member on a value that is neither\n");
    }
    return map;
}

/* A class of the PROGRAM that implements `Map`, `Set` or `Map.Entry` answers what a comparison
   with it would ask -- its `size`, its `entries`, its `key` -- only through members this runtime
   has no way to call. Comparing with one fails loudly, naming it, rather than answering a guess. */
static void kt_fail_foreign(const char *what, size_t what_length, KRef value) {
    static const char before[] = "krusty: comparing with the program's ";
    static const char after[] = ", whose members this runtime cannot call\n";
    kt_sys_write(2, before, sizeof(before) - 1);
    kt_sys_write(2, what, what_length);
    kt_sys_write(2, " ", 1);
    kt_sys_write(2, value->header.type->name, value->header.type->name_length);
    kt_sys_fail(after, sizeof(after) - 1);
}

#define KT_FAIL_FOREIGN(what, value) kt_fail_foreign(what, sizeof(what) - 1, value)

/* ---- the table ------------------------------------------------------------------------------

   `HashMap`'s own members, each written as Kotlin/Native writes it. A call into the program -- a
   key's `hashCode` or `equals`, a value's `equals` -- may raise, and Kotlin leaves the member at
   once with the map as it stands at that point; so each such call is followed by a look at the
   pending slot, and a raise returns before the next statement Kotlin would not have reached. Every
   read of the map's arrays goes through the map, as Kotlin's reads of its properties do, because a
   call into the program may have replaced them. */

enum {
    /* `MAGIC`, the golden ratio's 32 bits, `2654435769L.toInt()`. */
    KT_MAGIC = -1640531527,
    KT_INITIAL_CAPACITY = 8,
    KT_INITIAL_MAX_PROBE_DISTANCE = 2,
    KT_TOMBSTONE = -1
};

static kt_boolean kt_map_raised(void) { return kt_pending_exception() != NULL; }

static KRef *kt_refs(KRef array) { return kt_elements_of(array); }
static kt_int *kt_ints(KRef array) { return (kt_int *)((KArray *)array + 1); }

static kt_int kt_capacity(const KMap *map) { return kt_length_of(map->keys); }
static kt_int kt_hash_size(const KMap *map) { return kt_length_of(map->hashes); }

kt_int kt_map_size(KRef self) {
    return kt_is_empty_one(self) ? 0 : kt_map_of_receiver(self)->size;
}

kt_boolean kt_map_is_empty(KRef self) { return kt_map_size(self) == 0; }

/* The size of a map, a set or a view, or -1 for anything else. */
kt_int kt_map_collection_size(KRef value) {
    if (kt_is_empty_one(value)) {
        return 0;
    }
    KMap *map = kt_map_behind(value);
    return map == NULL ? -1 : map->size;
}

/* `computeHashSize`: the highest power of two in three times the capacity (at least one). */
static kt_int kt_compute_hash_size(kt_int capacity) {
    uint32_t wanted = (uint32_t)(capacity < 1 ? 1 : capacity) * 3u;
    uint32_t size = 1;
    while (size <= wanted / 2) {
        size <<= 1;
    }
    return (kt_int)size;
}

/* `computeShift`: the leading zero bits of the hash array's size, plus one. */
static kt_int kt_compute_shift(kt_int hash_size) {
    kt_int zeros = 0;
    for (uint32_t bit = 0x80000000u; bit != 0 && ((uint32_t)hash_size & bit) == 0; bit >>= 1) {
        zeros++;
    }
    return zeros + 1;
}

/* `hash(key)`: zero for null, else the key's `hashCode` times `MAGIC`, shifted down to the hash
   array's range. False when the `hashCode` raised. */
static kt_boolean kt_hash_of(const KMap *map, KRef key, kt_int *hash) {
    if (key == NULL) {
        *hash = 0;
        return true;
    }
    kt_int code = kt_hash_code(key);
    if (kt_map_raised()) {
        return false;
    }
    *hash = (kt_int)(((uint32_t)code * (uint32_t)KT_MAGIC) >> map->hash_shift);
    return true;
}

/* A copy of `array` `length` long, the tail NULL or zero: `copyOfUninitializedElements` and
   `copyOf`. */
static KRef kt_copy_of(KRef array, kt_int length) {
    KRef copy = kt_array_new(array->header.type, length);
    kt_int kept = kt_length_of(array) < length ? kt_length_of(array) : length;
    memcpy((void *)((KArray *)copy + 1), (const void *)((const KArray *)array + 1),
           (size_t)kept * array->header.type->element_size);
    return copy;
}

/* `resetRange(from, to)`: the references in it cleared. */
static void kt_reset_range(KRef array, kt_int from, kt_int to) {
    for (kt_int at = from; at < to; at++) {
        kt_refs(array)[at] = NULL;
    }
}

static KRef kt_map_shaped(const KType *type, kt_int capacity) {
    KRef keys = kt_array_new(&kt_type_array, capacity);
    KRef presence = kt_array_new(&kt_type_int_array, capacity);
    kt_int hash_size = kt_compute_hash_size(capacity);
    KRef hashes = kt_array_new(&kt_type_int_array, hash_size);
    KMap *map = (KMap *)kt_gc_allocate(type, sizeof(KMap));
    map->keys = keys;
    map->presence = presence;
    map->hashes = hashes;
    map->max_probe_distance = KT_INITIAL_MAX_PROBE_DISTANCE;
    map->hash_shift = kt_compute_shift(hash_size);
    return (KRef)map;
}

static KRef *kt_values_of(KMap *map) {
    if (map->values == NULL) {
        map->values = kt_array_new(&kt_type_array, kt_capacity(map));
    }
    return kt_refs(map->values);
}

static void kt_compact(KMap *map, kt_boolean update_hash_array) {
    kt_int j = 0;
    for (kt_int i = 0; i < map->length; i++) {
        kt_int hash = kt_ints(map->presence)[i];
        if (hash >= 0) {
            kt_refs(map->keys)[j] = kt_refs(map->keys)[i];
            if (map->values != NULL) {
                kt_refs(map->values)[j] = kt_refs(map->values)[i];
            }
            if (update_hash_array) {
                kt_ints(map->presence)[j] = hash;
                kt_ints(map->hashes)[hash] = j + 1;
            }
            j++;
        }
    }
    kt_reset_range(map->keys, j, map->length);
    if (map->values != NULL) {
        kt_reset_range(map->values, j, map->length);
    }
    map->length = j;
}

/* `putRehash(i)`: key `i` into the fresh hash array, false when its probe would go too far. */
static kt_boolean kt_put_rehash(KMap *map, kt_int i) {
    kt_int hash = 0;
    if (!kt_hash_of(map, kt_refs(map->keys)[i], &hash)) {
        return false;
    }
    kt_int probes_left = map->max_probe_distance;
    for (;;) {
        if (kt_ints(map->hashes)[hash] == 0) {
            kt_ints(map->hashes)[hash] = i + 1;
            kt_ints(map->presence)[i] = hash;
            return true;
        }
        if (--probes_left < 0) {
            return false;
        }
        if (hash-- == 0) {
            hash = kt_hash_size(map) - 1;
        }
    }
}

/* `rehash(newHashSize)`: a structural change, the holes compacted away, and every key hashed again
   into a fresh hash array -- a `hashCode` call per key. */
static void kt_rehash(KMap *map, kt_int new_hash_size) {
    map->modifications++;
    if (map->length > map->size) {
        kt_compact(map, false);
    }
    map->hashes = kt_array_new(&kt_type_int_array, new_hash_size);
    map->hash_shift = kt_compute_shift(new_hash_size);
    for (kt_int i = 0; i < map->length; i++) {
        if (!kt_put_rehash(map, i)) {
            if (!kt_map_raised()) {
                static const char message[] = "This cannot happen with fixed magic multiplier and "
                                              "grow-only hash array. Have object hashCodes changed?";
                kt_throw(kt_throwable_new(&kt_type_illegal_state_exception,
                                          kt_string_utf8(message, sizeof(message) - 1)));
            }
            return;
        }
    }
}

/* `AbstractList.newCapacity`: half again as many, at least `minimum`. */
static kt_int kt_new_capacity(kt_int old_capacity, kt_int minimum) {
    const kt_int max_array_size = 0x7fffffff - 8;
    kt_int grown = (kt_int)((uint32_t)old_capacity + (uint32_t)(old_capacity >> 1));
    if ((kt_int)((uint32_t)grown - (uint32_t)minimum) < 0) {
        grown = minimum;
    }
    if ((kt_int)((uint32_t)grown - (uint32_t)max_array_size) > 0) {
        grown = minimum > max_array_size ? 0x7fffffff : max_array_size;
    }
    return grown;
}

static void kt_ensure_capacity(KMap *map, kt_int minimum) {
    if (minimum < 0) {
        kt_fail_oom();
    }
    if (minimum <= kt_capacity(map)) {
        return;
    }
    kt_int size = kt_new_capacity(kt_capacity(map), minimum);
    map->keys = kt_copy_of(map->keys, size);
    if (map->values != NULL) {
        map->values = kt_copy_of(map->values, size);
    }
    map->presence = kt_copy_of(map->presence, size);
    kt_int new_hash_size = kt_compute_hash_size(size);
    if (new_hash_size > kt_hash_size(map)) {
        kt_rehash(map, new_hash_size);
    }
}

/* `ensureExtraCapacity(n)`: compact the holes away when that makes the room and they are a quarter
   of the capacity, else grow. */
static void kt_ensure_extra_capacity(KMap *map, kt_int extra) {
    kt_int spare = kt_capacity(map) - map->length;
    kt_int gaps = map->length - map->size;
    if (spare < extra && gaps + spare >= extra && gaps >= kt_capacity(map) / 4) {
        kt_compact(map, true);
    } else {
        kt_ensure_capacity(map, map->length + extra);
    }
}

/* `findKey`: the index of `key`, -1 for none and when a call raised. */
static kt_int kt_find_key(KMap *map, KRef key) {
    kt_int hash = 0;
    if (!kt_hash_of(map, key, &hash)) {
        return KT_TOMBSTONE;
    }
    kt_int probes_left = map->max_probe_distance;
    for (;;) {
        kt_int index = kt_ints(map->hashes)[hash];
        if (index == 0) {
            return KT_TOMBSTONE;
        }
        kt_boolean same = kt_equals(kt_refs(map->keys)[index - 1], key);
        if (kt_map_raised()) {
            return KT_TOMBSTONE;
        }
        if (same) {
            return index - 1;
        }
        if (--probes_left < 0) {
            return KT_TOMBSTONE;
        }
        if (hash-- == 0) {
            hash = kt_hash_size(map) - 1;
        }
    }
}

/* `findValue`: the LAST index whose value the stored value's `equals` finds `value`, -1 for none
   and when a call raised. */
static kt_int kt_find_value(KMap *map, KRef value) {
    for (kt_int i = map->length - 1; i >= 0; i--) {
        if (kt_ints(map->presence)[i] < 0) {
            continue;
        }
        kt_boolean same = kt_equals(kt_refs(map->values)[i], value);
        if (kt_map_raised()) {
            return KT_TOMBSTONE;
        }
        if (same) {
            return i;
        }
    }
    return KT_TOMBSTONE;
}

/* `addKey`: the index a new key took, or minus one more than the index of the key already there.
   The caller asks the pending slot. */
static kt_int kt_add_key(KMap *map, KRef key) {
    for (;;) {
        kt_int hash = 0;
        if (!kt_hash_of(map, key, &hash)) {
            return 0;
        }
        kt_int doubled = map->max_probe_distance * 2;
        kt_int tentative = doubled < kt_hash_size(map) / 2 ? doubled : kt_hash_size(map) / 2;
        kt_int probe_distance = 0;
        kt_boolean retry = false;
        while (!retry) {
            kt_int index = kt_ints(map->hashes)[hash];
            if (index == 0) {
                if (map->length >= kt_capacity(map)) {
                    kt_ensure_extra_capacity(map, 1);
                    if (kt_map_raised()) {
                        return 0;
                    }
                    retry = true;
                    continue;
                }
                kt_int put = map->length++;
                kt_refs(map->keys)[put] = key;
                kt_ints(map->presence)[put] = hash;
                kt_ints(map->hashes)[hash] = put + 1;
                map->size++;
                map->modifications++;
                if (probe_distance > map->max_probe_distance) {
                    map->max_probe_distance = probe_distance;
                }
                return put;
            }
            kt_boolean same = kt_equals(kt_refs(map->keys)[index - 1], key);
            if (kt_map_raised()) {
                return 0;
            }
            if (same) {
                return -index;
            }
            if (++probe_distance > tentative) {
                kt_rehash(map, kt_hash_size(map) * 2);
                if (kt_map_raised()) {
                    return 0;
                }
                retry = true;
                continue;
            }
            if (hash-- == 0) {
                hash = kt_hash_size(map) - 1;
            }
        }
    }
}

/* `removeHashAt`: close the hole a removed slot leaves, moving a later key of the probe sequence
   into it when its own slot is further back -- which asks that key its `hashCode`. */
static void kt_remove_hash_at(KMap *map, kt_int removed_hash) {
    kt_int hash = removed_hash;
    kt_int hole = removed_hash;
    kt_int probe_distance = 0;
    for (;;) {
        if (hash-- == 0) {
            hash = kt_hash_size(map) - 1;
        }
        kt_int index = kt_ints(map->hashes)[hash];
        if (++probe_distance > map->max_probe_distance || index == 0) {
            kt_ints(map->hashes)[hole] = 0;
            return;
        }
        kt_int other_hash = 0;
        if (!kt_hash_of(map, kt_refs(map->keys)[index - 1], &other_hash)) {
            return;
        }
        if ((kt_int)((uint32_t)(other_hash - hash) & (uint32_t)(kt_hash_size(map) - 1)) >=
            probe_distance) {
            kt_ints(map->hashes)[hole] = index;
            kt_ints(map->presence)[index - 1] = hole;
            hole = hash;
            probe_distance = 0;
        }
    }
}

/* `removeEntryAt`. */
static void kt_remove_entry_at(KMap *map, kt_int index) {
    kt_refs(map->keys)[index] = NULL;
    if (map->values != NULL) {
        kt_refs(map->values)[index] = NULL;
    }
    kt_remove_hash_at(map, kt_ints(map->presence)[index]);
    if (kt_map_raised()) {
        return;
    }
    kt_ints(map->presence)[index] = KT_TOMBSTONE;
    map->size--;
    map->modifications++;
}

/* `removeKey`: whether `key` was there to remove. */
static kt_boolean kt_remove_key(KMap *map, KRef key) {
    kt_int index = kt_find_key(map, key);
    if (index < 0) {
        return false;
    }
    kt_remove_entry_at(map, index);
    return !kt_map_raised();
}

kt_boolean kt_map_contains_value(KRef self, KRef value) {
    return !kt_is_empty_one(self) && kt_find_value(kt_map_of_receiver(self), value) >= 0;
}

/* `clear()`: a structural change even when there is nothing to clear. */
void kt_map_clear(KRef self) {
    KMap *map = kt_map_of_receiver(self);
    for (kt_int i = 0; i < map->length; i++) {
        kt_int hash = kt_ints(map->presence)[i];
        if (hash >= 0) {
            kt_ints(map->hashes)[hash] = 0;
            kt_ints(map->presence)[i] = KT_TOMBSTONE;
        }
    }
    kt_reset_range(map->keys, 0, map->length);
    if (map->values != NULL) {
        kt_reset_range(map->values, 0, map->length);
    }
    map->size = 0;
    map->length = 0;
    map->modifications++;
}

/* `HashMap(initialCapacity)` and `HashSet(initialCapacity)`: Kotlin/Native's exception for a
   negative one, with the JVM's message. */
static KRef kt_map_with_capacity(const KType *type, kt_int capacity) {
    if (capacity < 0) {
        static const char message[] = "Illegal initial capacity: ";
        KRef text =
            kt_string_plus(kt_string_utf8(message, sizeof(message) - 1), kt_box_int(capacity));
        kt_throw(kt_throwable_new(&kt_type_illegal_argument_exception, text));
        return NULL;
    }
    return kt_map_shaped(type, capacity);
}

KRef kt_map_new(void) { return kt_map_shaped(&kt_type_hash_map, KT_INITIAL_CAPACITY); }
KRef kt_set_new(void) { return kt_map_shaped(&kt_type_hash_set, KT_INITIAL_CAPACITY); }
KRef kt_hash_map_new(void) { return kt_map_new(); }
KRef kt_hash_set_new(void) { return kt_set_new(); }

KRef kt_hash_map_with_capacity(kt_int capacity) {
    return kt_map_with_capacity(&kt_type_hash_map, capacity);
}

KRef kt_hash_set_with_capacity(kt_int capacity) {
    return kt_map_with_capacity(&kt_type_hash_set, capacity);
}

/* ---- iteration ------------------------------------------------------------------------------

   `HashMap.Itr`: `hasNext` is whether the index is below the map's length; `next` first checks
   that the map has not changed structurally since the iterator was made --
   `ConcurrentModificationException` if it has -- then hands out the index and moves past the
   holes after it. */

static void kt_init_next(KMapIterator *iterator) {
    const KMap *map = (const KMap *)iterator->map;
    while (iterator->index < map->length && kt_ints(map->presence)[iterator->index] < 0) {
        iterator->index++;
    }
}

static void kt_iterator_start(KMapIterator *iterator, KMap *map) {
    iterator->map = (KRef)map;
    iterator->index = 0;
    iterator->last = -1;
    iterator->expected = map->modifications;
    kt_init_next(iterator);
}

static kt_boolean kt_iterator_more(const KMapIterator *iterator) {
    return iterator->index < ((const KMap *)iterator->map)->length;
}

/* The next index; -1 with the exception pending when the map changed or nothing is left. */
static kt_int kt_iterator_next_index(KMapIterator *iterator) {
    const KMap *map = (const KMap *)iterator->map;
    if (map->modifications != iterator->expected) {
        kt_throw(kt_throwable_new(&kt_type_concurrent_modification_exception, NULL));
        return -1;
    }
    if (iterator->index >= map->length) {
        kt_throw(kt_throwable_new(&kt_type_no_such_element_exception, NULL));
        return -1;
    }
    iterator->last = iterator->index++;
    return iterator->last;
}

/* `EntryRef(map, index)`. */
static KRef kt_entry_ref(KMap *map, kt_int index) {
    KMapEntry *entry = (KMapEntry *)kt_gc_allocate(&kt_type_map_entry_ref, sizeof(KMapEntry));
    entry->map = (KRef)map;
    entry->index = index;
    entry->expected = map->modifications;
    return (KRef)entry;
}

/* What an iterator of `type` hands out at `index`. */
static KRef kt_yielded(const KType *type, KMap *map, kt_int index) {
    if (type == &kt_type_keys_iterator) {
        return kt_refs(map->keys)[index];
    }
    if (type == &kt_type_values_iterator) {
        return kt_refs(map->values)[index];
    }
    return kt_entry_ref(map, index);
}

/* The iterator class for a map, a set or a view: a map's and `entries`' entries, `values`' values,
   and a set's or `keys`' keys. */
static const KType *kt_iterator_type(KRef self) {
    const KType *type = self->header.type;
    return type == &kt_type_hash_map || type == &kt_type_entries_view ? &kt_type_entries_iterator
           : type == &kt_type_values_view                            ? &kt_type_values_iterator
                                                                     : &kt_type_keys_iterator;
}

static KRef kt_map_walk(KRef self) {
    KMap *map = kt_map_of_receiver(self);
    KMapIterator *iterator =
        (KMapIterator *)kt_gc_allocate(kt_iterator_type(self), sizeof(KMapIterator));
    kt_iterator_start(iterator, map);
    return (KRef)iterator;
}

static kt_boolean kt_map_iterator_has_next(KRef self) {
    return kt_iterator_more((const KMapIterator *)self);
}

/* `next()`: the element, found before the holes after it are skipped. */
static KRef kt_step(KMapIterator *iterator, const KType *type) {
    kt_int index = kt_iterator_next_index(iterator);
    if (index < 0) {
        return NULL;
    }
    KRef result = kt_yielded(type, (KMap *)iterator->map, index);
    kt_init_next(iterator);
    return result;
}

static KRef kt_map_iterator_next(KRef self) {
    return kt_step((KMapIterator *)self, self->header.type);
}

/* What a map, a set or a view holds, in iteration order, as a fresh list: a map's keys, and what
   a set or a view yields. */
KRef kt_map_keys_list(KRef self) {
    if (kt_is_empty_one(self)) {
        return kt_mutable_list_new();
    }
    KMap *map = kt_map_of_receiver(self);
    const KType *type =
        self->header.type == &kt_type_hash_map ? &kt_type_keys_iterator : kt_iterator_type(self);
    KRef growing = kt_mutable_list_new();
    KMapIterator walk;
    kt_iterator_start(&walk, map);
    while (kt_iterator_more(&walk)) {
        KRef element = kt_step(&walk, type);
        if (kt_map_raised()) {
            return NULL;
        }
        kt_mutable_list_add(growing, element);
    }
    return growing;
}

/* ---- map members ---------------------------------------------------------------------------- */

kt_boolean kt_map_contains_key(KRef self, KRef key) {
    return !kt_is_empty_one(self) && kt_find_key(kt_map_of_receiver(self), key) >= 0;
}

/* `m[k]`, NULL for an absent key. */
KRef kt_map_get(KRef self, KRef key) {
    if (kt_is_empty_one(self)) {
        return NULL;
    }
    KMap *map = kt_map_of_receiver(self);
    kt_int index = kt_find_key(map, key);
    return index < 0 ? NULL : kt_refs(map->values)[index];
}

KRef kt_map_get_or_default(KRef self, KRef key, KRef fallback) {
    if (kt_is_empty_one(self)) {
        return fallback;
    }
    KMap *map = kt_map_of_receiver(self);
    kt_int index = kt_find_key(map, key);
    if (kt_map_raised()) {
        return NULL;
    }
    return index < 0 ? fallback : kt_refs(map->values)[index];
}

/* `m.put(k, v)`, answering the value that was there. An existing key keeps its index, and so its
   place in the order, and the key object it went in with. */
KRef kt_map_put(KRef self, KRef key, KRef value) {
    KMap *map = kt_map_of_receiver(self);
    kt_int index = kt_add_key(map, key);
    if (kt_map_raised()) {
        return NULL;
    }
    KRef *values = kt_values_of(map);
    if (index < 0) {
        KRef previous = values[-index - 1];
        values[-index - 1] = value;
        return previous;
    }
    values[index] = value;
    return NULL;
}

void kt_map_set(KRef self, KRef key, KRef value) { (void)kt_map_put(self, key, value); }

KRef kt_map_remove(KRef self, KRef key) {
    KMap *map = kt_map_of_receiver(self);
    kt_int index = kt_find_key(map, key);
    if (index < 0) {
        return NULL;
    }
    KRef previous = kt_refs(map->values)[index];
    kt_remove_entry_at(map, index);
    return kt_map_raised() ? NULL : previous;
}

/* A view, made once per map and kept. */
static KRef kt_map_view(KMap *map, KRef *slot, const KType *type) {
    if (*slot == NULL) {
        KMapView *view = (KMapView *)kt_gc_allocate(type, sizeof(KMapView));
        view->map = (KRef)map;
        *slot = (KRef)view;
    }
    return *slot;
}

KRef kt_map_keys(KRef self) {
    if (kt_is_empty_one(self)) {
        return kt_set_empty();
    }
    KMap *map = kt_map_of_receiver(self);
    return kt_map_view(map, &map->keys_view, &kt_type_keys_view);
}

KRef kt_map_values(KRef self) {
    if (kt_is_empty_one(self)) {
        return kt_list_empty();
    }
    KMap *map = kt_map_of_receiver(self);
    return kt_map_view(map, &map->values_view, &kt_type_values_view);
}

KRef kt_map_entries(KRef self) {
    if (kt_is_empty_one(self)) {
        return kt_set_empty();
    }
    KMap *map = kt_map_of_receiver(self);
    return kt_map_view(map, &map->entries_view, &kt_type_entries_view);
}

/* ---- entries -------------------------------------------------------------------------------- */

/* An entry of the runtime's own, for `entries` to look up; a class of the program implementing
   `Map.Entry` fails loudly, and anything else is NULL. */
static KMapEntry *kt_entry_argument(KRef value) {
    if (!kt_is_instance(value, &kt_type_map_entry)) {
        return NULL;
    }
    if (value->header.type != &kt_type_map_entry_ref) {
        KT_FAIL_FOREIGN("Map.Entry", value);
    }
    return (KMapEntry *)value;
}

/* `EntryRef.checkForComodification`: false, with Kotlin/Native's exception and the JVM-less
   message it carries, once the map has changed structurally since the entry was made. */
static kt_boolean kt_entry_current(const KMapEntry *entry) {
    if (((const KMap *)entry->map)->modifications == entry->expected) {
        return true;
    }
    static const char message[] = "The backing map has been modified after this entry was obtained.";
    kt_throw(kt_throwable_new(&kt_type_concurrent_modification_exception,
                              kt_string_utf8(message, sizeof(message) - 1)));
    return false;
}

KRef kt_map_entry_key(KRef entry) {
    const KMapEntry *self = (const KMapEntry *)entry;
    return kt_entry_current(self) ? kt_refs(((const KMap *)self->map)->keys)[self->index] : NULL;
}

KRef kt_map_entry_value(KRef entry) {
    const KMapEntry *self = (const KMapEntry *)entry;
    return kt_entry_current(self) ? kt_refs(((const KMap *)self->map)->values)[self->index] : NULL;
}

/* `EntryRef.equals`: an entry whose key equals this one's and whose value equals this one's, each
   asked of the OTHER entry's half -- `other.key == key` -- and stopping at a call that throws. */
static kt_boolean kt_entry_equals(KRef self, KRef other) {
    KMapEntry *entry = kt_entry_argument(other);
    if (entry == NULL) {
        return false;
    }
    KRef their_key = kt_map_entry_key((KRef)entry);
    if (kt_map_raised()) {
        return false;
    }
    KRef key = kt_map_entry_key(self);
    if (kt_map_raised() || !kt_equals(their_key, key) || kt_map_raised()) {
        return false;
    }
    KRef their_value = kt_map_entry_value((KRef)entry);
    if (kt_map_raised()) {
        return false;
    }
    KRef value = kt_map_entry_value(self);
    if (kt_map_raised()) {
        return false;
    }
    kt_boolean same = kt_equals(their_value, value);
    return same && !kt_map_raised();
}

/* `key.hashCode() xor value.hashCode()`, stopping at a call that throws. */
static kt_int kt_entry_hash_code(KRef self) {
    KRef key = kt_map_entry_key(self);
    kt_int key_hash = kt_map_raised() ? 0 : kt_hash_code(key);
    if (kt_map_raised()) {
        return 0;
    }
    KRef value = kt_map_entry_value(self);
    kt_int value_hash = kt_map_raised() ? 0 : kt_hash_code(value);
    return kt_map_raised() ? 0 : key_hash ^ value_hash;
}

/* `"$key=$value"`; a half that throws answers no text. */
static KRef kt_entry_to_string(KRef self) {
    KRef key = kt_map_entry_key(self);
    KRef key_text = kt_map_raised() ? NULL : kt_to_string(key);
    if (kt_map_raised()) {
        return NULL;
    }
    KRef value = kt_map_entry_value(self);
    KRef value_text = kt_map_raised() ? NULL : kt_to_string(value);
    if (kt_map_raised()) {
        return NULL;
    }
    return kt_string_plus(kt_string_plus(key_text, kt_string_utf8("=", 1)), value_text);
}

/* `containsEntry`: the index of the entry's key, whose stored value must then equal the entry's
   -- `valuesArray[index] == entry.value`. */
static kt_boolean kt_contains_entry(KMap *map, KMapEntry *entry, kt_int *found) {
    *found = KT_TOMBSTONE;
    KRef key = kt_map_entry_key((KRef)entry);
    if (kt_map_raised()) {
        return false;
    }
    kt_int index = kt_find_key(map, key);
    if (index < 0) {
        return false;
    }
    KRef value = kt_map_entry_value((KRef)entry);
    if (kt_map_raised()) {
        return false;
    }
    kt_boolean same = kt_equals(kt_refs(map->values)[index], value);
    if (kt_map_raised() || !same) {
        return false;
    }
    *found = index;
    return true;
}

/* ---- set members, for a set and for each view ------------------------------------------------ */

kt_boolean kt_set_contains(KRef self, KRef value) {
    if (kt_is_empty_one(self)) {
        return false;
    }
    KMap *map = kt_map_of_receiver(self);
    const KType *type = self->header.type;
    if (type == &kt_type_values_view) {
        return kt_find_value(map, value) >= 0;
    }
    if (type == &kt_type_entries_view) {
        KMapEntry *entry = kt_entry_argument(value);
        kt_int index = 0;
        return entry != NULL && kt_contains_entry(map, entry, &index);
    }
    return kt_find_key(map, value) >= 0;
}

/* `add`: a view has none, `UnsupportedOperationException`. A set's is its map's `addKey`, which
   answers whether the key was new. */
kt_boolean kt_set_add(KRef self, KRef value) {
    KMap *map = kt_map_of_receiver(self);
    if (kt_is_view_type(self->header.type)) {
        kt_throw(kt_throwable_new(&kt_type_unsupported_operation_exception, NULL));
        return false;
    }
    kt_int index = kt_add_key(map, value);
    return !kt_map_raised() && index >= 0;
}

/* `remove` on a set, and through a view onto its map: a key by `removeKey`, a value by
   `removeValue` -- the LAST index holding it -- and an entry by `removeEntry`, when the entry's key
   is there and its stored value equals the entry's. */
kt_boolean kt_set_remove(KRef self, KRef value) {
    KMap *map = kt_map_of_receiver(self);
    const KType *type = self->header.type;
    kt_int index = KT_TOMBSTONE;
    if (type == &kt_type_values_view) {
        index = kt_find_value(map, value);
    } else if (type == &kt_type_entries_view) {
        KMapEntry *entry = kt_entry_argument(value);
        if (entry == NULL || !kt_contains_entry(map, entry, &index)) {
            return false;
        }
    } else {
        return kt_remove_key(map, value);
    }
    if (index < 0) {
        return false;
    }
    kt_remove_entry_at(map, index);
    return !kt_map_raised();
}

/* `mapOf(a to b, …)`, `hashMapOf(…)`, `setOf(a, …)`, `hashSetOf(…)` of at least one item, and
   `hashMapOf` and `hashSetOf` of none: Kotlin/Native's `mapCapacity` is the count itself, so a map
   of that capacity, filled in order. The array belongs
   to the caller and is copied from. An element whose `hashCode` or `equals` throws ends the build
   there. */
static KRef kt_map_filled(const KType *type, KRef items, kt_boolean are_pairs) {
    kt_int length = kt_length_of(items);
    KRef map = kt_map_shaped(type, length);
    for (kt_int at = 0; at < length; at++) {
        KRef item = kt_elements_of(items)[at];
        if (are_pairs) {
            (void)kt_map_put(map, kt_pair_first(item), kt_pair_second(item));
        } else {
            (void)kt_set_add(map, item);
        }
        if (kt_map_raised()) {
            return map;
        }
    }
    return map;
}

/* `mapOf(vararg pairs)` is `if (pairs.size > 0) pairs.toMap(LinkedHashMap(mapCapacity(pairs.size)))
   else emptyMap()`, and `setOf(vararg elements)` is `elements.toSet()`, which answers `emptySet()`
   for none. `hashMapOf` and `hashSetOf` fill a new `HashMap` or `HashSet` whatever the count. */
KRef kt_map_of(KRef pairs) {
    return kt_length_of(pairs) == 0 ? kt_map_empty() : kt_map_filled(&kt_type_hash_map, pairs, true);
}
KRef kt_set_of(KRef elements) {
    return kt_length_of(elements) == 0 ? kt_set_empty()
                                       : kt_map_filled(&kt_type_hash_set, elements, false);
}
KRef kt_hash_map_of(KRef pairs) { return kt_map_filled(&kt_type_hash_map, pairs, true); }
KRef kt_hash_set_of(KRef elements) { return kt_map_filled(&kt_type_hash_set, elements, false); }

/* `mapOf(a to b)`: Kotlin/Native's is `hashMapOf(a to b)`, a map of capacity one. */
KRef kt_map_of_pair(KRef pair) {
    KRef map = kt_map_shaped(&kt_type_hash_map, 1);
    (void)kt_map_put(map, kt_pair_first(pair), kt_pair_second(pair));
    return map;
}

/* ---- equality, hashing and rendering --------------------------------------------------------

   `HashMap`'s own, and `AbstractMutableSet`'s and `AbstractCollection`'s for a set and the views.
   Each stops at the first call into the program that throws. */

/* Equal to itself, or to any `Map` of the same size each of whose entries this map contains:
   `contentEquals`, which walks the OTHER map's `entries` and looks each up here -- this map's
   stored key's `equals`, then its stored value's. A `ClassCastException` from the lookup answers
   false, as `containsAllEntries` catches it. */
static kt_boolean kt_contains_all_entries(KMap *map, KRef other) {
    KMapIterator walk;
    KMap *theirs = kt_map_behind(other);
    kt_iterator_start(&walk, theirs);
    while (kt_iterator_more(&walk)) {
        KRef element = kt_step(&walk, kt_iterator_type(other));
        if (kt_map_raised()) {
            return false;
        }
        KMapEntry *entry = kt_entry_argument(element);
        if (entry == NULL) {
            return false;
        }
        kt_int index = 0;
        kt_boolean held = kt_contains_entry(map, entry, &index);
        if (kt_map_raised()) {
            if (kt_is_instance(kt_pending_exception(), &kt_type_class_cast_exception)) {
                kt_clear_pending();
            }
            return false;
        }
        if (!held) {
            return false;
        }
    }
    return true;
}

static kt_boolean kt_map_equals(KRef self, KRef other) {
    if (self == other) {
        return true;
    }
    if (!kt_is_instance(other, &kt_type_map_interface)) {
        return false;
    }
    if (!kt_is_map(other)) {
        KT_FAIL_FOREIGN("Map", other);
    }
    KMap *map = (KMap *)self;
    /* `EmptyMap`'s `entries` hold nothing to look up: the sizes answer. */
    if (kt_is_empty_one(other)) {
        return map->size == 0;
    }
    if (map->size != ((const KMap *)other)->size) {
        return false;
    }
    return kt_contains_all_entries(map, kt_map_entries(other));
}

/* The sum of `key.hashCode() xor value.hashCode()` over the entries, read by index with no check
   for a change, as `EntriesItr.nextHashCode` reads them. */
static kt_int kt_map_hash_code(KRef self) {
    KMap *map = (KMap *)self;
    KMapIterator walk;
    kt_iterator_start(&walk, map);
    uint32_t total = 0;
    while (kt_iterator_more(&walk)) {
        kt_int index = walk.index++;
        kt_int key = kt_hash_code(kt_refs(map->keys)[index]);
        if (kt_map_raised()) {
            return 0;
        }
        kt_int value = kt_hash_code(kt_refs(map->values)[index]);
        if (kt_map_raised()) {
            return 0;
        }
        total += (uint32_t)(key ^ value);
        kt_init_next(&walk);
    }
    return (kt_int)total;
}

/* One part of a collection as its `toString` shows it: the collection itself as Kotlin's marker,
   `(this Map)` or `(this Collection)`, anything else through its own `toString`; NULL when that
   threw. */
static KRef kt_part_to_string(KRef self, KRef part, const char *marker, kt_int marker_length) {
    if (part == self) {
        return kt_string_utf8(marker, marker_length);
    }
    KRef text = kt_to_string(part);
    return kt_map_raised() ? NULL : text;
}

#define KT_THIS_MAP "(this Map)", (kt_int)(sizeof("(this Map)") - 1)
#define KT_THIS_COLLECTION "(this Collection)", (kt_int)(sizeof("(this Collection)") - 1)

/* `{k=v, …}`, read by index with no check for a change, as `EntriesItr.nextAppendString` reads
   it. */
static KRef kt_map_to_string(KRef self) {
    KMap *map = (KMap *)self;
    KMapIterator walk;
    kt_iterator_start(&walk, map);
    KRef text = kt_string_utf8("{", 1);
    for (kt_int count = 0; kt_iterator_more(&walk); count++) {
        if (count > 0) {
            text = kt_string_plus(text, kt_string_utf8(", ", 2));
        }
        kt_int index = walk.index++;
        KRef key = kt_part_to_string(self, kt_refs(map->keys)[index], KT_THIS_MAP);
        if (key == NULL) {
            return NULL;
        }
        text = kt_string_plus(kt_string_plus(text, key), kt_string_utf8("=", 1));
        KRef value = kt_part_to_string(self, kt_refs(map->values)[index], KT_THIS_MAP);
        if (value == NULL) {
            return NULL;
        }
        text = kt_string_plus(text, value);
        kt_init_next(&walk);
    }
    return kt_string_plus(text, kt_string_utf8("}", 1));
}

/* Equal to itself, or to any `Set` of the same size all of whose elements this one contains:
   `AbstractSet.setEquals`, which walks the OTHER set and asks this one -- `entries` through
   `containsAllEntries`. */
static kt_boolean kt_set_equals(KRef self, KRef other) {
    if (self == other) {
        return true;
    }
    if (!kt_is_instance(other, &kt_type_set_interface)) {
        return false;
    }
    if (!kt_is_set(other)) {
        KT_FAIL_FOREIGN("Set", other);
    }
    if (kt_map_size(self) != kt_map_size(other)) {
        return false;
    }
    /* `EmptySet` holds nothing to ask this set about. */
    if (kt_is_empty_one(other)) {
        return true;
    }
    if (self->header.type == &kt_type_entries_view) {
        return kt_contains_all_entries(kt_map_behind(self), other);
    }
    KMapIterator walk;
    kt_iterator_start(&walk, kt_map_behind(other));
    while (kt_iterator_more(&walk)) {
        KRef element = kt_step(&walk, kt_iterator_type(other));
        if (kt_map_raised()) {
            return false;
        }
        kt_boolean held = kt_set_contains(self, element);
        if (kt_map_raised() || !held) {
            return false;
        }
    }
    return true;
}

/* `AbstractSet.unorderedHashCode`: the sum of the elements' hashes, each checked before it is
   added. */
static kt_int kt_set_hash_code(KRef self) {
    KMapIterator walk;
    kt_iterator_start(&walk, kt_map_behind(self));
    uint32_t total = 0;
    while (kt_iterator_more(&walk)) {
        KRef element = kt_step(&walk, kt_iterator_type(self));
        if (kt_map_raised()) {
            return 0;
        }
        kt_int hash = kt_hash_code(element);
        if (kt_map_raised()) {
            return 0;
        }
        total += (uint32_t)hash;
    }
    return (kt_int)total;
}

/* `[a, b]`, as `AbstractCollection` renders a set or a view. */
static KRef kt_set_to_string(KRef self) {
    KMapIterator walk;
    kt_iterator_start(&walk, kt_map_behind(self));
    KRef text = kt_string_utf8("[", 1);
    for (kt_int count = 0; kt_iterator_more(&walk); count++) {
        KRef element = kt_step(&walk, kt_iterator_type(self));
        if (kt_map_raised()) {
            return NULL;
        }
        if (count > 0) {
            text = kt_string_plus(text, kt_string_utf8(", ", 2));
        }
        KRef part = kt_part_to_string(self, element, KT_THIS_COLLECTION);
        if (part == NULL) {
            return NULL;
        }
        text = kt_string_plus(text, part);
    }
    return kt_string_plus(text, kt_string_utf8("]", 1));
}

/* `EmptyMap.equals`: `other is Map<*, *> && other.isEmpty()`. A map of the program's would be asked
   its `isEmpty`, which this runtime cannot call. */
static kt_boolean kt_empty_map_equals(KRef self, KRef other) {
    (void)self;
    if (!kt_is_instance(other, &kt_type_map_interface)) {
        return false;
    }
    if (!kt_is_map(other)) {
        KT_FAIL_FOREIGN("Map", other);
    }
    return kt_map_size(other) == 0;
}

/* `EmptySet.equals`: `other is Set<*> && other.isEmpty()`, with the same limit. */
static kt_boolean kt_empty_set_equals(KRef self, KRef other) {
    (void)self;
    if (!kt_is_instance(other, &kt_type_set_interface)) {
        return false;
    }
    if (!kt_is_set(other)) {
        KT_FAIL_FOREIGN("Set", other);
    }
    return kt_map_size(other) == 0;
}

static kt_int kt_empty_hash_code(KRef self) {
    (void)self;
    return 0;
}

static KRef kt_empty_map_to_string(KRef self) {
    (void)self;
    return kt_string_utf8("{}", 2);
}

static KRef kt_empty_set_to_string(KRef self) {
    (void)self;
    return kt_string_utf8("[]", 2);
}

/* `EmptySet.iterator()`, and the `Map.iterator()` extension on `EmptyMap`, `entries.iterator()`. */
static KRef kt_empty_walk(KRef self) {
    (void)self;
    return &kt_the_empty_iterator;
}

static kt_boolean kt_empty_iterator_has_next(KRef self) {
    (void)self;
    return false;
}

/* `EmptyIterator.next()`: `throw NoSuchElementException()`, with no message. */
static KRef kt_empty_iterator_next(KRef self) {
    (void)self;
    kt_throw(kt_throwable_new(&kt_type_no_such_element_exception, NULL));
    return NULL;
}

#undef KT_THIS_COLLECTION
#undef KT_THIS_MAP
#undef KT_FAIL_FOREIGN

/* The companion object of a BUILT-IN type.

   Each is declared in no file krusty compiles and carries no state: every member of one is a
   constant the frontend folds. So the only thing a program can observe is its IDENTITY, which the
   corpus does — `o === Int.Companion`, and `Int` written as a value is the same object as
   `Int.Companion`. One static object per companion gives exactly that: static storage, so the
   collector never sees it as an allocation and the address is stable for the program's life, the
   same way `kt_unit()` is.

   A DESCRIPTOR of its own per companion, never one shared: `Int.Companion === Long.Companion` must
   be false, and a shared type would also make `is` answer for the wrong one. `kotlin.Any`'s vtable,
   because a companion overrides none of the three. */
#define KT_COMPANION(suffix, owner)                                                                \
    const KType kt_type_##suffix##_companion = {KT_NAMED("kotlin." owner ".", "Companion"),        \
                                                .instance_size = sizeof(KObject),                  \
                                                .super = &kt_type_any,                             \
                                                .vtable = kt_any_vtable,                           \
                                                .vtable_length = 3};                               \
    KRef kt_##suffix##_companion(void) {                                                           \
        static KObject object = {{&kt_type_##suffix##_companion}, {{NULL, NULL, 0}}};              \
        return &object;                                                                            \
    }

KT_COMPANION(byte, "Byte")
KT_COMPANION(short, "Short")
KT_COMPANION(int, "Int")
KT_COMPANION(long, "Long")
KT_COMPANION(char, "Char")
KT_COMPANION(boolean, "Boolean")
KT_COMPANION(float, "Float")
KT_COMPANION(double, "Double")
KT_COMPANION(string, "String")
KT_COMPANION(ubyte, "UByte")
KT_COMPANION(ushort, "UShort")
KT_COMPANION(uint, "UInt")
KT_COMPANION(ulong, "ULong")

#undef KT_COMPANION

/* Static storage, not the heap: the collector never sees it as an object, and nothing needs it
   to. */
KRef kt_unit(void) {
    static KObject unit = {{&kt_type_unit}, {{NULL, NULL, 0}}};
    return &unit;
}
