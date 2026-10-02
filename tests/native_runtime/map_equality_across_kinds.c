/* Map and set equality is Kotlin/Native's: a map equals any `Map` and a set any `Set` of the same
   size and contents, whatever its class, and each is equal to itself without asking anything. A
   map walks the OTHER map's entries and looks each up in itself, asking its stored key's and then
   its stored value's `equals` (`HashMap.contentEquals`); a set walks the other set and asks itself
   (`AbstractSet.setEquals`); an entry equals any `Map.Entry` whose key and value, asked first,
   equal its own. A `ClassCastException` thrown inside a map's lookup is taken back and answers
   false, as `containsAllEntries` catches it; any other exception propagates.

   Equality used to hold only between two objects of the same runtime class, walked the receiver
   for sets as well, compared a collection with itself element by element, and let every exception
   through.

   The driver prints what each comparison or hash answered, or what it raised, and the calls it
   made, and the harness compares the lines with what `map_equality_across_kinds.kt` answers
   under the reference kotlinc, whose `K` is `logged_keys.h`'s and whose `Throws` and `Acting` are
   this driver's. The JVM's `AbstractMap` and `AbstractSet` walk and ask the other way round, take
   back a `NullPointerException` too, and fail a walk across a change; those lines are declared
   divergences. */
#include "logged_keys.h"
#include "transcript.h"

/* `Throws`: a `Key` whose `n` says what its `equals` throws. */
static kt_boolean throws_equals(KRef self, KRef other);

static const kt_fn throws_vtable[] = {(kt_fn)throws_equals, (kt_fn)key_hash_code,
                                      (kt_fn)key_to_string};

static const KType throws_type = {
    .name = "Throws",
    .name_length = sizeof("Throws") - 1,
    .instance_size = sizeof(Key),
    .super = &kt_type_any,
    .vtable = throws_vtable,
    .vtable_length = 3,
};

static kt_boolean throws_equals(KRef self, KRef other) {
    (void)other;
    const Key *key = (const Key *)self;
    log_text("throw(");
    log_text(key->tag);
    log_text(") ");
    const KType *thrown = key->n == 0   ? &kt_type_null_pointer_exception
                          : key->n == 1 ? &kt_type_class_cast_exception
                                        : &kt_type_illegal_state_exception;
    kt_throw(kt_throwable_new(thrown, NULL));
    return 1;
}

/* `Acting`: an `equals` that first runs `action`, once. */
static void (*action)(void);
static KRef acted_on;

static kt_boolean acting_equals(KRef self, KRef other) {
    void (*pending)(void) = action;
    action = NULL;
    if (pending != NULL) {
        pending();
    }
    return other != NULL && type_of(other) == type_of(self) &&
           ((const Key *)other)->n == ((const Key *)self)->n;
}

static const kt_fn acting_vtable[] = {(kt_fn)acting_equals, (kt_fn)key_hash_code,
                                      (kt_fn)key_to_string};

static const KType acting_type = {
    .name = "Acting",
    .name_length = sizeof("Acting") - 1,
    .instance_size = sizeof(Key),
    .super = &kt_type_any,
    .vtable = acting_vtable,
    .vtable_length = 3,
};

static KRef text(const char *bytes) {
    kt_int length = 0;
    while (bytes[length] != 0) {
        length++;
    }
    return kt_string_utf8(bytes, length);
}

static void put_z(void) { kt_map_set(acted_on, text("z"), text("1")); }

static KRef first_of(KRef iterable) { return kt_iterator_next(kt_iterable_iterator(iterable)); }

/* `label = answer | calls`, the answer being what the step raised when it raised, and the calls
   those made since the last step. */
static void step(const char *label, KRef answer) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    say(label);
    say(" = ");
    if (thrown != NULL) {
        say_thrown(thrown);
    } else {
        say_value(answer);
    }
    say(" | ");
    say_bytes(call_log, call_log_length);
    say("\n");
    (void)logged("");
}

static KRef yes_no(kt_boolean value) { return kt_box_boolean(value); }

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_gc_add_global_root((void **)&acted_on);

    KRef a = key(1, 7, "a");
    KRef b = key(2, 7, "b");
    KRef va = key(10, 1, "va");
    KRef vb = key(20, 2, "vb");
    KRef m1 = kt_map_new();
    kt_map_set(m1, a, va);
    kt_map_set(m1, b, vb);
    KRef a2 = key(1, 7, "a2");
    KRef b2 = key(2, 7, "b2");
    KRef va2 = key(10, 1, "va2");
    KRef vb2 = key(20, 2, "vb2");
    KRef m2 = kt_hash_map_new();
    kt_map_set(m2, b2, vb2);
    kt_map_set(m2, a2, va2);
    (void)logged("");

    step("m1 == m2", yes_no(kt_equals(m1, m2)));
    step("m2 == m1", yes_no(kt_equals(m2, m1)));
    step("m1 == m1", yes_no(kt_equals(m1, m1)));
    step("m1.hashCode()", kt_box_int(kt_hash_code(m1)));

    KRef n1 = kt_map_new();
    kt_map_set(n1, a, NULL);
    KRef n2 = kt_hash_map_new();
    kt_map_set(n2, a2, NULL);
    KRef n3 = kt_hash_map_new();
    kt_map_set(n3, b2, NULL);
    (void)logged("");
    step("n1 == n2", yes_no(kt_equals(n1, n2)));
    step("n1 == n3", yes_no(kt_equals(n1, n3)));

    KRef s1 = kt_set_new();
    (void)kt_set_add(s1, a);
    (void)kt_set_add(s1, b);
    KRef s2 = kt_hash_set_new();
    (void)kt_set_add(s2, b2);
    (void)kt_set_add(s2, a2);
    (void)logged("");
    step("s1 == s2", yes_no(kt_equals(s1, s2)));
    step("s2 == s1", yes_no(kt_equals(s2, s1)));
    step("s1 == s1", yes_no(kt_equals(s1, s1)));
    step("s1.hashCode()", kt_box_int(kt_hash_code(s1)));
    step("s1 == m1.keys", yes_no(kt_equals(s1, kt_map_keys(m1))));
    step("m1.keys == s1", yes_no(kt_equals(kt_map_keys(m1), s1)));
    step("m1.entries == m2.entries", yes_no(kt_equals(kt_map_entries(m1), kt_map_entries(m2))));
    step("m1.entries.hashCode()", kt_box_int(kt_hash_code(kt_map_entries(m1))));

    KRef e1 = first_of(kt_map_entries(m1));
    KRef e2 = kt_hash_map_new();
    kt_map_set(e2, a2, va2);
    e2 = first_of(kt_map_entries(e2));
    (void)logged("");
    step("e1 == e2", yes_no(kt_equals(e1, e2)));
    step("e1.hashCode()", kt_box_int(kt_hash_code(e1)));
    step("e1 == Pair(a, va)", yes_no(kt_equals(e1, kt_pair_of(a, va))));
    step("m1 == s1", yes_no(kt_equals(m1, s1)));
    KRef elements = kt_array_new(&kt_type_array, 2);
    ((KRef *)((char *)elements + kt_type_array.instance_size))[0] = a;
    ((KRef *)((char *)elements + kt_type_array.instance_size))[1] = b;
    step("s1 == listOf(a, b)", yes_no(kt_equals(s1, kt_list_of(elements))));

    KRef pm = kt_map_new();
    kt_map_set(pm, key_of(&throws_type, 0, 7, "x"), text("1"));
    KRef pm2 = kt_map_new();
    kt_map_set(pm2, key_of(&throws_type, 0, 7, "y"), text("1"));
    (void)logged("");
    step("pm == pm2", yes_no(kt_equals(pm, pm2)));
    KRef cm = kt_map_new();
    kt_map_set(cm, text("k"), key_of(&throws_type, 1, 7, "v"));
    KRef cm2 = kt_map_new();
    kt_map_set(cm2, text("k"), text("5"));
    (void)logged("");
    step("cm == cm2", yes_no(kt_equals(cm, cm2)));
    KRef ps = kt_set_new();
    (void)kt_set_add(ps, key_of(&throws_type, 0, 7, "s"));
    KRef ps2 = kt_set_new();
    (void)kt_set_add(ps2, key_of(&throws_type, 0, 7, "t"));
    (void)logged("");
    step("ps == ps2", yes_no(kt_equals(ps, ps2)));
    KRef im = kt_map_new();
    kt_map_set(im, text("k"), key_of(&throws_type, 2, 7, "i"));
    (void)logged("");
    step("im == cm2", yes_no(kt_equals(im, cm2)));

    KRef c1 = kt_map_new();
    kt_map_set(c1, text("k"), key_of(&acting_type, 1, 7, "c1k"));
    kt_map_set(c1, text("l"), key_of(&acting_type, 2, 7, "c1l"));
    KRef c2 = kt_map_new();
    kt_map_set(c2, text("k"), key_of(&acting_type, 1, 7, "c2k"));
    kt_map_set(c2, text("l"), key_of(&acting_type, 2, 7, "c2l"));
    acted_on = c1;
    action = put_z;
    step("c1 == c2", yes_no(kt_equals(c1, c2)));

    CHECK(kt_pending_exception() == NULL, "an equality raised\n");
    kt_sys_write(1, "OK\n", 3);
}
