/* Every map and set iterates in insertion order, as Kotlin/Native's `HashMap` and `HashSet` do:
   they keep their keys in an array in insertion order, a removed key leaving a hole and a re-added
   one going to the end, and `LinkedHashMap` and `LinkedHashSet` are type aliases for them. A
   negative initial capacity is Kotlin's `IllegalArgumentException`, with the JVM's message.

   The driver prints each order, rendered by the runtime, and the harness compares the lines with
   what `map_iteration_order.kt` answers under the reference kotlinc, whose `H` the driver's `Key`
   stands in for. The JVM's `HashMap` and `HashSet` iterate in the order of their table instead; each
   such line is a declared divergence. */
#include "logged_keys.h"
#include "transcript.h"

/* `h(n)`: a `Key` whose hash is `n` and whose text is `n` in decimal. */
static char texts[64][12];
static int texts_used;

static KRef h_of(kt_int n, kt_int hash) {
    char *text = texts[texts_used++];
    uint32_t magnitude = n < 0 ? 0u - (uint32_t)n : (uint32_t)n;
    char digits[12];
    int count = 0;
    do {
        digits[count++] = (char)('0' + magnitude % 10u);
        magnitude /= 10u;
    } while (magnitude != 0u);
    int at = 0;
    if (n < 0) {
        text[at++] = '-';
    }
    while (count > 0) {
        text[at++] = digits[--count];
    }
    text[at] = 0;
    return key(n, hash, text);
}

static KRef h(kt_int n) { return h_of(n, n); }

/* An `Array<Any?>` of the given elements, as a vararg call packs one. */
static KRef array_of(int count, const KRef *elements) {
    KRef array = kt_array_new(&kt_type_array, count);
    KRef *slots = (KRef *)((char *)array + kt_type_array.instance_size);
    for (int at = 0; at < count; at++) {
        slots[at] = elements[at];
    }
    return array;
}

static KRef text(const char *bytes) {
    kt_int length = 0;
    while (bytes[length] != 0) {
        length++;
    }
    return kt_string_utf8(bytes, length);
}

static void show(KRef collection) {
    say_value(collection);
    say("\n");
}

/* `HashMap(capacity)` or `HashSet(capacity)` of a negative capacity: what it raised. */
static void show_refused(KRef made) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    CHECK(made == NULL && thrown != NULL, "a negative capacity answered a map\n");
    say_thrown(thrown);
    say("\n");
}

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    DRIVER_BEGIN();
    KRef v = text("v");

    KRef hm = kt_hash_map_new();
    static const kt_int hm_keys[] = {33, 1, 17, 16, 0, 49, 2};
    for (int at = 0; at < 7; at++) {
        kt_map_set(hm, h(hm_keys[at]), v);
    }
    show(kt_map_keys(hm));

    KRef fruit[] = {kt_pair_of(text("banana"), v), kt_pair_of(text("apple"), v),
                    kt_pair_of(text("cherry"), v), kt_pair_of(text("date"), v)};
    show(kt_map_keys(kt_hash_map_of(array_of(4, fruit))));

    KRef numbers[] = {h(100), h(3), h(17), h(64), h(5)};
    show(kt_hash_set_of(array_of(5, numbers)));

    KRef big = kt_hash_map_new();
    for (kt_int i = 0; i < 13; i++) {
        kt_map_set(big, h(i * 16), v);
    }
    show(kt_map_keys(big));

    KRef hs = kt_hash_set_new();
    static const kt_int hs_keys[] = {15, 31, 47, 63, 1};
    for (int at = 0; at < 5; at++) {
        (void)kt_set_add(hs, h(hs_keys[at]));
    }
    show(hs);

    KRef coll = kt_hash_map_new();
    for (kt_int i = 0; i < 10; i++) {
        kt_map_set(coll, h_of(i, 5 + 32 * i), v);
    }
    show(kt_map_keys(coll));

    KRef spread[] = {h(-1), h(-16), h(65536), h(65537), h(1)};
    show(kt_hash_set_of(array_of(5, spread)));

    KRef letters[] = {kt_pair_of(h(1), text("a")), kt_pair_of(h(2), text("b")),
                      kt_pair_of(h(3), text("c"))};
    KRef rm = kt_hash_map_of(array_of(3, letters));
    (void)kt_map_remove(rm, h(1));
    kt_map_set(rm, h(1), text("z"));
    kt_map_set(rm, h(17), text("q"));
    show(kt_map_keys(rm));

    KRef linked[] = {kt_pair_of(h(17), text("c")), kt_pair_of(h(1), text("a"))};
    show(kt_map_keys(kt_map_of(array_of(2, linked))));

    KRef zero = kt_hash_map_with_capacity(0);
    kt_map_set(zero, h(5), v);
    kt_map_set(zero, h(1), v);
    kt_map_set(zero, h(3), v);
    show(kt_map_keys(zero));

    KRef two = kt_hash_set_with_capacity(2);
    (void)kt_set_add(two, h(9));
    (void)kt_set_add(two, h(2));
    (void)kt_set_add(two, h(4));
    show(two);

    KRef mutable_pairs[] = {kt_pair_of(h(9), v), kt_pair_of(h(2), v)};
    show(kt_map_keys(kt_map_of(array_of(2, mutable_pairs))));

    KRef set_elements[] = {h(33), h(1), h(17)};
    show(kt_set_of(array_of(3, set_elements)));

    show_refused(kt_hash_map_with_capacity(-1));
    show_refused(kt_hash_set_with_capacity(-7));

    CHECK(kt_pending_exception() == NULL, "an ordering raised\n");
    kt_sys_write(1, "OK\n", 3);
}
