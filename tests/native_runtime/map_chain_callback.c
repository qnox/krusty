/* A key's or a value's `equals` is the program's code, and may clear the very map that called it,
   or clear and refill it. Kotlin/Native's `HashMap` reads its hash array and its keys afresh at
   every probe, and `clear()` empties both: a lookup whose first comparison cleared the map finds
   the next slot empty and answers null, one that refilled it finds the refilled key; a removal
   finds nothing to remove; a `put` claims the slot after the one it compared, where a later lookup,
   starting at the key's own slot, does not reach it; and `containsValue`, walking the values from
   the last back, compares the entry it reached first.

   The driver prints each answer, the map, and its size, and the harness compares the lines with
   what `map_chain_callback.kt` answers under the reference kotlinc, where `K` and `V` are the
   program classes this driver stands in for. The JVM's `HashMap` walks a bucket's chain, which
   `clear()` leaves linked, and answers otherwise; those lines are declared divergences. */
#include "driver_exceptions.h"
#include "transcript.h"

/* `K` and `V`: an `n` its `equals` compares, a hash (7 for every `K`), and a text. */
typedef struct Acting {
    KObjectHeader header;
    kt_int n;
    kt_int h;
    const char *tag;
} Acting;

/* What the next `equals` of a `K` or a `V` does before it answers, once. */
static void (*action)(void);
static KRef target;

static void act(void) {
    void (*pending)(void) = action;
    action = NULL;
    if (pending != NULL) {
        pending();
    }
}

static kt_boolean acting_equals(KRef self, KRef other);
static kt_int acting_hash_code(KRef self) { return ((const Acting *)self)->h; }

static kt_int length_of(const char *text) {
    kt_int length = 0;
    while (text[length] != 0) {
        length++;
    }
    return length;
}

static KRef text(const char *bytes) { return kt_string_utf8(bytes, length_of(bytes)); }

static KRef acting_to_string(KRef self) { return text(((const Acting *)self)->tag); }

static const kt_fn acting_vtable[] = {(kt_fn)acting_equals, (kt_fn)acting_hash_code,
                                      (kt_fn)acting_to_string};

static const KType k_type = {
    .name = "K",
    .name_length = 1,
    .instance_size = sizeof(Acting),
    .super = &kt_type_any,
    .vtable = acting_vtable,
    .vtable_length = 3,
};

static const KType v_type = {
    .name = "V",
    .name_length = 1,
    .instance_size = sizeof(Acting),
    .super = &kt_type_any,
    .vtable = acting_vtable,
    .vtable_length = 3,
};

static kt_boolean acting_equals(KRef self, KRef other) {
    act();
    return other != NULL && type_of(other) == type_of(self) &&
           ((const Acting *)other)->n == ((const Acting *)self)->n;
}

static KRef acting(const KType *type, kt_int n, kt_int h, const char *tag) {
    Acting *made = (Acting *)kt_gc_allocate(type, sizeof(Acting));
    made->n = n;
    made->h = h;
    made->tag = tag;
    return (KRef)made;
}

static KRef k(kt_int n, const char *tag) { return acting(&k_type, n, 7, tag); }
static KRef v(kt_int n, const char *tag) { return acting(&v_type, n, n, tag); }

static void clear_target(void) { kt_map_clear(target); }

static void refill_keys(void) {
    kt_map_clear(target);
    kt_map_set(target, k(3, "c"), text("C"));
    kt_map_set(target, k(2, "d"), text("D"));
}

static void refill_values(void) {
    kt_map_clear(target);
    kt_map_set(target, k(4, "e"), v(4, "z"));
}

static KRef (*maker)(void);

/* A fresh map of the kind being driven, `{a=A, b=B}`, the target of the next action. */
static KRef fresh(void) {
    target = maker();
    kt_map_set(target, k(1, "a"), text("A"));
    kt_map_set(target, k(2, "b"), text("B"));
    return target;
}

/* `kind label: answer map size=n`. */
static void line(const char *kind, const char *label, KRef answer, KRef map) {
    CHECK(kt_pending_exception() == NULL, "a map raised\n");
    say(kind);
    say(" ");
    say(label);
    say(": ");
    say_value(answer);
    say(" ");
    say_value(map);
    say(" size=");
    say_long(kt_map_size(map));
    say("\n");
}

static void drive(const char *kind) {
    KRef m = fresh();
    action = clear_target;
    line(kind, "get clearing", kt_map_get(m, k(2, "q")), m);

    m = fresh();
    action = refill_keys;
    line(kind, "get clearing and refilling", kt_map_get(m, k(2, "q")), m);
    line(kind, "get after refilling", kt_map_get(m, k(2, "q")), m);

    m = fresh();
    action = clear_target;
    line(kind, "containsKey clearing", kt_box_boolean(kt_map_contains_key(m, k(2, "q"))), m);

    m = fresh();
    action = clear_target;
    line(kind, "remove clearing", kt_map_remove(m, k(2, "q")), m);

    m = fresh();
    action = clear_target;
    line(kind, "put clearing", kt_map_put(m, k(3, "c"), text("C")), m);
    line(kind, "get after put clearing", kt_map_get(m, k(3, "c")), m);

    m = fresh();
    kt_map_set(m, k(1, "a"), v(1, "x"));
    kt_map_set(m, k(2, "b"), v(2, "y"));
    action = clear_target;
    line(kind, "containsValue clearing", kt_box_boolean(kt_map_contains_value(m, v(2, "w"))), m);

    m = fresh();
    kt_map_set(m, k(1, "a"), v(1, "x"));
    kt_map_set(m, k(2, "b"), v(2, "y"));
    action = refill_values;
    line(kind, "containsValue clearing and refilling",
         kt_box_boolean(kt_map_contains_value(m, v(2, "w"))), m);
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_gc_add_global_root((void **)&target);
    maker = kt_hash_map_new;
    drive("HashMap");
    maker = kt_map_new;
    drive("LinkedHashMap");
    kt_sys_write(1, "OK\n", 3);
}
