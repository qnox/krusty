/* Each iterator a map, a set or a view hands out is Kotlin/Native's class for it: `HashMap`'s
   `KeysItr`, `ValuesItr` and `EntriesItr` under the open `HashMap.Itr`, for either spelling of a map
   or a set; a set's is its map's key iterator, and a map's own its entries'. They used to share one
   anonymous class.

   The driver prints each iterator's class, and the harness compares the lines with what
   `map_iterator_identity.kt` answers under the reference kotlinc. The JVM's six `java.util`
   classes differ; each line is a declared divergence. */
#include "driver_exceptions.h"
#include "transcript.h"

static void show(const char *label, KRef iterator) {
    KRef literal = kt_class_of(iterator);
    say(label);
    say(": ");
    say_value(kt_class_qualified_name(literal));
    say(" ");
    say_value(kt_class_simple_name(literal));
    say(" super=");
    const KType *parent = type_of(iterator)->super;
    CHECK(parent != NULL, "an iterator class has no superclass\n");
    if (parent->qualified_name == NULL) {
        say("null");
    } else {
        say_bytes(parent->qualified_name, (kt_int)parent->qualified_name_length);
    }
    say(" ");
    say_bool(kt_is_instance(iterator, &kt_type_iterator_interface));
    say("\n");
}

static KRef one_to_a(KRef map) {
    kt_map_set(map, kt_box_int(1), kt_string_utf8("a", 1));
    return map;
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef hash = one_to_a(kt_hash_map_new());
    KRef linked = one_to_a(kt_map_new());
    show("HashMap", kt_iterable_iterator(hash));
    show("HashMap.keys", kt_iterable_iterator(kt_map_keys(hash)));
    show("HashMap.values", kt_iterable_iterator(kt_map_values(hash)));
    show("HashMap.entries", kt_iterable_iterator(kt_map_entries(hash)));
    show("LinkedHashMap", kt_iterable_iterator(linked));
    show("LinkedHashMap.keys", kt_iterable_iterator(kt_map_keys(linked)));
    show("LinkedHashMap.values", kt_iterable_iterator(kt_map_values(linked)));
    show("LinkedHashMap.entries", kt_iterable_iterator(kt_map_entries(linked)));
    KRef hash_set = kt_hash_set_new();
    (void)kt_set_add(hash_set, kt_box_int(1));
    show("HashSet", kt_iterable_iterator(hash_set));
    KRef linked_set = kt_set_new();
    (void)kt_set_add(linked_set, kt_box_int(1));
    show("LinkedHashSet", kt_iterable_iterator(linked_set));
    show("empty HashMap.keys", kt_iterable_iterator(kt_map_keys(kt_hash_map_new())));
    CHECK(kt_pending_exception() == NULL, "making an iterator raised\n");
    kt_sys_write(1, "OK\n", 3);
}
