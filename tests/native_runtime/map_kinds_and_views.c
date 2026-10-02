/* The classes a map is made of, and its views, as Kotlin/Native has them.

   `HashMap` and `HashSet` are the two classes, `LinkedHashMap` and `LinkedHashSet` type aliases for
   them, named as `::class` names them there. An entry is a `kotlin.collections.HashMap.EntryRef`
   that reads the map by index, so it shows a later `put` of its key; `Map.Entry` and
   `MutableMap.MutableEntry` are only the interfaces it implements. `keys`, `values` and `entries`
   are live views, the same object every time: `keys` and `entries` are sets, `values` a collection
   that is no `List` and equals only itself. Removing through a view removes from the map; adding
   through one is `UnsupportedOperationException`. An iterator fails with
   `ConcurrentModificationException` once the map changed structurally under it, before it would
   fail with `NoSuchElementException`, and a value written over an existing key is no such change;
   `hasNext` is whether its index is below the map's length, which `clear()` makes zero. `value in
   set`, walked as an `Iterable`, is the set's own `contains`.

   The driver prints what each step shows, answers or raises, and the harness compares the lines
   with what `map_kinds_and_views.kt` answers under the reference kotlinc, whose `K` is
   `logged_keys.h`'s. The JVM's four classes, its entry and view classes, its `hasNext` after a
   `clear()` and its `contains`, which asks the incoming element's `equals`, differ; those lines are
   declared divergences. */
#include "logged_keys.h"
#include "transcript.h"

static KRef text(const char *bytes) {
    kt_int length = 0;
    while (bytes[length] != 0) {
        length++;
    }
    return kt_string_utf8(bytes, length);
}

static void sp(void) { say(" "); }
static void nl(void) { say("\n"); }

/* A value's class's names: `qualified simple`. */
static void say_names(KRef value) {
    KRef literal = kt_class_of(value);
    say_value(kt_class_qualified_name(literal));
    sp();
    say_value(kt_class_simple_name(literal));
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
    nl();
}

static KRef first_of(KRef iterable) { return kt_iterator_next(kt_iterable_iterator(iterable)); }

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef m = kt_map_new();
    kt_map_set(m, text("a"), text("1"));
    kt_map_set(m, text("b"), text("2"));
    KRef ks = kt_map_keys(m);
    KRef vs = kt_map_values(m);
    KRef es = kt_map_entries(m);
    say_bool(ks == kt_map_keys(m));
    sp();
    say_bool(vs == kt_map_values(m));
    sp();
    say_bool(es == kt_map_entries(m));
    nl();
    kt_map_set(m, text("c"), text("3"));
    say_value(ks);
    sp();
    say_value(vs);
    sp();
    say_value(es);
    sp();
    say_long(kt_map_size(ks));
    sp();
    say_long(kt_map_size(vs));
    sp();
    say_long(kt_map_size(es));
    nl();

    KRef e = first_of(es);
    kt_map_set(m, text("a"), text("10"));
    say_value(e);
    sp();
    say_value(kt_map_entry_key(e));
    sp();
    say_value(kt_map_entry_value(e));
    nl();
    say_names(e);
    sp();
    say_names(ks);
    sp();
    say_names(vs);
    sp();
    say_names(es);
    nl();

    KRef h = kt_hash_map_new();
    kt_map_set(h, text("x"), text("1"));
    say_names(first_of(kt_map_entries(h)));
    sp();
    say_names(kt_map_keys(h));
    sp();
    say_names(kt_map_values(h));
    sp();
    say_names(kt_map_entries(h));
    nl();

    KRef numbers = kt_array_new(&kt_type_array, 3);
    ((KRef *)((KArray *)numbers + 1))[0] = text("10");
    ((KRef *)((KArray *)numbers + 1))[1] = text("2");
    ((KRef *)((KArray *)numbers + 1))[2] = text("3");
    say_bool(kt_is_instance(vs, &kt_type_list_interface));
    sp();
    say_bool(kt_equals(vs, kt_list_of(numbers)));
    sp();
    say_bool(kt_equals(vs, vs));
    sp();
    say_bool(kt_is_instance(vs, &kt_type_collection_interface));
    sp();
    say_bool(kt_is_instance(ks, &kt_type_set_interface));
    sp();
    say_bool(kt_is_instance(es, &kt_type_set_interface));
    sp();
    say_bool(kt_is_instance(e, &kt_type_map_entry));
    sp();
    say_bool(kt_is_instance(e, &kt_type_mutable_map_entry));
    nl();

    KRef ls = kt_set_new();
    KRef hs = kt_hash_set_new();
    say_bool(kt_is_instance(m, &kt_type_hash_map));
    sp();
    say_bool(kt_is_instance(h, &kt_type_hash_map));
    sp();
    say_bool(kt_is_instance(ls, &kt_type_hash_set));
    sp();
    say_bool(kt_is_instance(hs, &kt_type_hash_set));
    sp();
    say_bool(kt_is_instance(m, &kt_type_mutable_map_interface));
    sp();
    say_bool(kt_is_instance(hs, &kt_type_mutable_set_interface));
    nl();
    say_names(m);
    sp();
    say_names(h);
    sp();
    say_names(hs);
    sp();
    say_names(ls);
    nl();

    step("keys.remove(b)", kt_box_boolean(kt_set_remove(ks, text("b"))));
    say_value(m);
    nl();
    step("values.remove(3)", kt_box_boolean(kt_set_remove(vs, text("3"))));
    say_value(m);
    nl();
    step("keys.add(z)", kt_box_boolean(kt_set_add(ks, text("z"))));
    step("values.add(4)", kt_box_boolean(kt_set_add(vs, text("4"))));
    say_value(m);
    nl();

    KRef it = kt_iterable_iterator(m);
    kt_map_set(m, text("q"), text("5"));
    step("next after a put", kt_iterator_next(it));
    KRef it2 = kt_iterable_iterator(kt_map_keys(m));
    while (kt_iterator_has_next(it2)) {
        (void)kt_iterator_next(it2);
    }
    kt_map_set(m, text("r"), text("6"));
    step("exhausted next after a put", kt_iterator_next(it2));
    KRef it3 = kt_iterable_iterator(kt_map_values(m));
    while (kt_iterator_has_next(it3)) {
        (void)kt_iterator_next(it3);
    }
    step("exhausted next", kt_iterator_next(it3));
    KRef it4 = kt_iterable_iterator(kt_map_keys(m));
    kt_map_set(m, text("a"), text("99"));
    step("next after an overwrite", kt_iterator_next(it4));
    KRef it5 = kt_iterable_iterator(kt_map_keys(m));
    kt_map_clear(m);
    say("hasNext after a clear: ");
    say_bool(kt_iterator_has_next(it5));
    nl();
    step("next after a clear", kt_iterator_next(it5));

    say_value(ks);
    sp();
    say_long(kt_map_size(ks));
    nl();
    kt_map_set(m, text("k"), text("1"));
    say_value(ks);
    sp();
    say_value(vs);
    sp();
    say_value(es);
    nl();
    KRef other = kt_hash_map_new();
    kt_map_set(other, text("k"), text("1"));
    KRef their_entry = first_of(kt_map_entries(other));
    say_bool(kt_set_contains(es, their_entry));
    sp();
    say_bool(kt_set_remove(es, their_entry));
    sp();
    say_value(m);
    nl();

    KRef loop = kt_map_new();
    kt_map_set(loop, text("x"), text("1"));
    kt_map_set(loop, text("y"), text("2"));
    KRef walk = kt_iterable_iterator(loop);
    while (kt_iterator_has_next(walk)) {
        KRef entry = kt_iterator_next(walk);
        say_value(kt_map_entry_key(entry));
        say_value(kt_map_entry_value(entry));
        sp();
    }
    nl();

    /* `value in set` walked as an `Iterable` is `Collection.contains`: the element's hash, then the
       elements its probe reaches, never an equality walk over all of them. */
    KRef keyed = kt_set_new();
    (void)kt_set_add(keyed, key(1, 7, "a"));
    (void)kt_set_add(keyed, key(2, 8, "b"));
    say("added | ");
    say_bytes(call_log, call_log_length);
    nl();
    call_log_length = 0;
    kt_boolean held = kt_iterable_contains(keyed, key(2, 8, "q"));
    say("in: ");
    say_bool(held);
    say(" | ");
    say_bytes(call_log, call_log_length);
    nl();
    call_log_length = 0;

    CHECK(kt_pending_exception() == NULL, "a view raised\n");
    kt_sys_write(1, "OK\n", 3);
}
