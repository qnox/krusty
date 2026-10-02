/* A key's `equals` or `toString` is the program's code, and may change the very map that called it.
   What the map does next is Kotlin/Native's:

   - a lookup whose comparison removed the key it matched reads the value the removal cleared,
     null, and one whose comparison removed another key finds the probe it would have taken empty;
   - a `put` whose comparison added a key compares with that one too;
   - `toString` reads the entries by index without checking for a change, so a key its `toString`
     added is rendered;
   - `containsValue` walks the values from the last back, so a comparison that removes an earlier
     entry leaves the one it compared in place.

   The driver prints each answer, or what the step raised, and the map afterwards, and the harness
   compares the lines with what `map_callback_mutation.kt` answers under the reference kotlinc,
   whose `K` this driver stands in for. Where the JVM's `HashMap` answers otherwise, the line is a
   declared divergence. */
#include "collections_later_tiers.h"
#include "transcript.h"

typedef struct Acting {
    KObjectHeader header;
    kt_int n;
    kt_int h;
    const char *tag;
} Acting;

/* What the next `equals` or `toString` does to a map before it answers, once. */
static void (*action)(void);
static KRef target;
static KRef extra_key;

static void run_action(void) {
    void (*pending)(void) = action;
    action = NULL;
    if (pending != NULL) {
        pending();
    }
}

static kt_boolean acting_equals(KRef self, KRef other);
static kt_int acting_hash_code(KRef self);
static KRef acting_to_string(KRef self);

static const kt_fn acting_vtable[] = {(kt_fn)acting_equals, (kt_fn)acting_hash_code,
                                      (kt_fn)acting_to_string};

static const KType acting_type = {
    .name = "K",
    .name_length = 1,
    .instance_size = sizeof(Acting),
    .super = &kt_type_any,
    .vtable = acting_vtable,
    .vtable_length = 3,
};

static KRef acting(kt_int n, const char *tag) {
    Acting *key = (Acting *)kt_gc_allocate(&acting_type, sizeof(Acting));
    key->n = n;
    key->h = 7;
    key->tag = tag;
    return (KRef)key;
}

static kt_boolean acting_equals(KRef self, KRef other) {
    run_action();
    return other != NULL && type_of(other) == &acting_type &&
           ((const Acting *)other)->n == ((const Acting *)self)->n;
}

static kt_int acting_hash_code(KRef self) { return ((const Acting *)self)->h; }

static KRef text(const char *bytes) {
    kt_int length = 0;
    while (bytes[length] != 0) {
        length++;
    }
    return kt_string_utf8(bytes, length);
}

static KRef acting_to_string(KRef self) {
    run_action();
    return text(((const Acting *)self)->tag);
}

/* `label: answer`, or what the step raised instead. */
static void step(const char *label, KRef answer) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    say(label);
    say(": ");
    if (thrown != NULL) {
        say_thrown(thrown);
    } else {
        say_value(answer);
    }
    say("\n");
}

static void remove_extra(void) { (void)kt_map_remove(target, extra_key); }
static void put_extra(void) { kt_map_set(target, extra_key, text("E")); }
static void put_y(void) { kt_map_set(target, text("y"), text("Y")); }

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_gc_add_global_root((void **)&target);
    kt_gc_add_global_root((void **)&extra_key);

    KRef a = acting(1, "a");
    KRef b = acting(2, "b");

    KRef m = kt_map_new();
    kt_map_set(m, a, text("A"));
    kt_map_set(m, b, text("B"));
    target = m;
    extra_key = a;
    action = remove_extra;
    step("LinkedHashMap get removing its match", kt_map_get(m, acting(1, "q")));
    step("then", m);

    KRef h = kt_hash_map_new();
    kt_map_set(h, a, text("A"));
    kt_map_set(h, b, text("B"));
    target = h;
    action = remove_extra;
    step("HashMap get removing another key", kt_map_get(h, acting(2, "q")));
    step("then", h);

    KRef p = kt_map_new();
    kt_map_set(p, a, text("A"));
    target = p;
    extra_key = acting(5, "e");
    action = put_extra;
    step("put adding a key", kt_map_put(p, acting(9, "n"), text("N")));
    step("then", p);

    KRef t = kt_map_new();
    kt_map_set(t, a, text("A"));
    kt_map_set(t, text("x"), text("X"));
    target = t;
    action = put_y;
    step("toString adding a key", kt_to_string(t));
    step("then", t);

    KRef cv = kt_map_new();
    kt_map_set(cv, text("k"), a);
    kt_map_set(cv, text("l"), b);
    target = cv;
    extra_key = text("k");
    action = remove_extra;
    step("LinkedHashMap containsValue removing its entry",
         kt_box_boolean(kt_map_contains_value(cv, acting(2, "w"))));
    step("then", cv);

    KRef cv2 = kt_hash_map_new();
    kt_map_set(cv2, text("k"), a);
    kt_map_set(cv2, text("l"), b);
    target = cv2;
    action = remove_extra;
    step("HashMap containsValue removing its entry",
         kt_box_boolean(kt_map_contains_value(cv2, acting(2, "w"))));
    step("then", cv2);

    CHECK(kt_pending_exception() == NULL, "a map raised\n");
    kt_sys_write(1, "OK\n", 3);
}
