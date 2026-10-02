/* What `mapOf` and `setOf` answer for an empty spread, as Kotlin/Native has it.

   Kotlin/Native's common `mapOf(vararg pairs)` is `emptyMap()` when there are no pairs, and
   `setOf(vararg elements)` is `elements.toSet()`, which is `emptySet()` for none: the stdlib's
   shared read-only `EmptyMap` and `EmptySet` objects (Maps.kt, Sets.kt, JetBrains/kotlin v2.4.10),
   one object each for the program's life. Neither is a `MutableMap` or `MutableSet`, so a cast to
   one fails and `as?` answers null. `equals` is `other is Map && other.isEmpty()` (`Set` for the
   set), `hashCode` 0, `toString` `{}` and `[]`; a lookup answers absent without asking the key
   anything; `keys` and `entries` are `EmptySet`; every iterator is `EmptyIterator`, whose `next`
   raises `NoSuchElementException`. `hashMapOf` and `hashSetOf` of an empty spread are a new
   `HashMap` and `HashSet` each time, which a write changes while the empty ones stay empty.

   The driver prints what each step shows, answers or raises, and the harness compares the lines
   with what `empty_map_and_set.kt` answers under the reference kotlinc, whose `K` is
   `logged_keys.h`'s. A failed cast keeps Kotlin/Native's wording, and the JVM's `HashMap` and
   `HashSet` are `java.util` classes; those lines are declared divergences. */
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

static KRef nothing(void) { return kt_array_new(&kt_type_array, 0); }

static KRef one_of(KRef item) {
    KRef array = kt_array_new(&kt_type_array, 1);
    ((KRef *)((KArray *)array + 1))[0] = item;
    return array;
}

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef m = kt_map_of(nothing());
    KRef s = kt_set_of(nothing());
    say("mapOf: ");
    say_names(m);
    nl();
    say("setOf: ");
    say_names(s);
    nl();
    say("same: ");
    say_bool(m == kt_map_of(nothing()));
    sp();
    say_bool(s == kt_set_of(nothing()));
    sp();
    say_bool(m == kt_map_empty());
    sp();
    say_bool(s == kt_set_empty());
    nl();

    say("is: ");
    say_bool(kt_is_instance(m, &kt_type_map_interface));
    sp();
    say_bool(kt_is_instance(m, &kt_type_mutable_map_interface));
    sp();
    say_bool(kt_is_instance(s, &kt_type_set_interface));
    sp();
    say_bool(kt_is_instance(s, &kt_type_collection_interface));
    sp();
    say_bool(kt_is_instance(s, &kt_type_iterable_interface));
    sp();
    say_bool(kt_is_instance(s, &kt_type_mutable_set_interface));
    sp();
    say_bool(kt_is_instance(s, &kt_type_mutable_collection_interface));
    sp();
    say_bool(kt_is_instance(s, &kt_type_mutable_iterable_interface));
    nl();
    say("as?: ");
    say_value(kt_safe_cast(m, &kt_type_mutable_map_interface));
    sp();
    say_value(kt_safe_cast(s, &kt_type_mutable_set_interface));
    nl();
    /* `(m as MutableMap).put(…)`: the cast raises, and the member is never called. */
    KRef writable = kt_cast(m, &kt_type_mutable_map_interface);
    step("put", kt_pending_exception() != NULL ? NULL : kt_map_put(writable, key(1, 1, "p"), text("1")));
    writable = kt_cast(s, &kt_type_mutable_set_interface);
    step("add", kt_pending_exception() != NULL ? NULL : kt_box_boolean(kt_set_add(writable, key(1, 1, "a"))));

    say("render: ");
    say_value(m);
    sp();
    say_value(s);
    sp();
    say_long(kt_hash_code(m));
    sp();
    say_long(kt_hash_code(s));
    nl();
    say("size: ");
    say_long(kt_map_size(m));
    sp();
    say_bool(kt_map_is_empty(m));
    sp();
    say_long(kt_map_size(s));
    sp();
    say_bool(kt_map_is_empty(s));
    nl();

    KRef k = key(1, 1, "k");
    say("lookup: ");
    say_bool(kt_map_contains_key(m, k));
    sp();
    say_value(kt_map_get(m, k));
    sp();
    say_value(kt_map_get_or_default(m, k, text("d")));
    sp();
    say_bool(kt_map_contains_value(m, text("v")));
    sp();
    say_bool(kt_set_contains(s, k));
    sp();
    say_bool(kt_iterable_contains(s, k));
    say(" | ");
    say_bytes(call_log, call_log_length);
    nl();
    call_log_length = 0;

    KRef values = kt_map_values(m);
    say("views: ");
    say_bool(kt_map_keys(m) == s);
    sp();
    say_bool(kt_map_entries(m) == s);
    sp();
    say_value(values);
    sp();
    say_long(kt_list_size(values));
    sp();
    say_bool(kt_list_is_empty(values));
    nl();

    KRef i = kt_iterable_iterator(s);
    say("iterator: ");
    say_names(i);
    sp();
    say_bool(i == kt_iterable_iterator(s));
    sp();
    say_bool(i == kt_iterable_iterator(m));
    sp();
    say_bool(kt_iterator_has_next(i));
    nl();
    step("next", kt_iterator_next(i));
    say("walk: ");
    for (KRef walk = kt_iterable_iterator(s); kt_iterator_has_next(walk);) {
        say_value(kt_iterator_next(walk));
    }
    for (KRef walk = kt_iterable_iterator(m); kt_iterator_has_next(walk);) {
        KRef entry = kt_iterator_next(walk);
        say_value(kt_map_entry_key(entry));
        say_value(kt_map_entry_value(entry));
    }
    say(".");
    nl();

    KRef hm = kt_hash_map_new();
    KRef hs = kt_hash_set_new();
    say("equals: ");
    say_bool(kt_equals(m, hm));
    sp();
    say_bool(kt_equals(hm, m));
    sp();
    say_bool(kt_equals(s, hs));
    sp();
    say_bool(kt_equals(hs, s));
    sp();
    say_bool(kt_equals(s, kt_map_keys(hm)));
    sp();
    say_bool(kt_equals(kt_map_keys(hm), s));
    sp();
    say_bool(kt_equals(kt_map_entries(hm), s));
    sp();
    say_bool(kt_equals(m, s));
    sp();
    say_bool(kt_equals(s, m));
    sp();
    say_bool(kt_equals(s, kt_list_empty()));
    nl();

    KRef one = kt_map_of_pair(kt_pair_of(k, text("1")));
    KRef single = kt_set_of(one_of(k));
    call_log_length = 0;
    say("unequal: ");
    say_bool(kt_equals(m, one));
    sp();
    say_bool(kt_equals(one, m));
    sp();
    say_bool(kt_equals(s, single));
    sp();
    say_bool(kt_equals(single, s));
    say(" | ");
    say_bytes(call_log, call_log_length);
    nl();
    call_log_length = 0;

    KRef hm0 = kt_hash_map_of(nothing());
    KRef hs0 = kt_hash_set_of(nothing());
    say("hashMapOf: ");
    say_names(hm0);
    sp();
    say_bool(hm0 == kt_hash_map_of(nothing()));
    sp();
    say_bool(kt_is_instance(hm0, &kt_type_mutable_map_interface));
    sp();
    say_bool(kt_equals(hm0, m));
    nl();
    say("hashSetOf: ");
    say_names(hs0);
    sp();
    say_bool(hs0 == kt_hash_set_of(nothing()));
    sp();
    say_bool(kt_is_instance(hs0, &kt_type_mutable_set_interface));
    sp();
    say_bool(kt_equals(hs0, s));
    nl();
    kt_map_set(hm0, k, text("1"));
    (void)kt_set_add(hs0, k);
    call_log_length = 0;
    say("written: ");
    say_value(hm0);
    sp();
    say_value(hs0);
    sp();
    say_value(m);
    sp();
    say_value(s);
    nl();

    CHECK(kt_pending_exception() == NULL, "an empty map or set raised\n");
    kt_sys_write(1, "OK\n", 3);
}
