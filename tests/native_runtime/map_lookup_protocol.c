/* The calls a lookup makes into the program, which are Kotlin/Native's `HashMap` protocol: the
   key's `hashCode` once, then, for each slot its probe reaches, the STORED key's `equals` of the
   key looked for, with no identity check. `containsValue` asks each stored value's `equals`, from
   the last entry back. The hash array exists from construction, so even a fresh map hashes the
   key it is asked for.

   The lookup used to compare every stored key by the stored key's `equals` without asking
   `hashCode`, walking every key.

   The driver prints what each step answered and the calls it made, and the harness compares the
   lines with what `map_lookup_protocol.kt` answers under the reference kotlinc, whose `K` and `A`
   are `logged_keys.h`'s. The JVM's `HashMap` asks the INCOMING key's `equals` after an identity
   check and hashes nothing for a map with no table yet; those lines are declared divergences. */
#include "logged_keys.h"
#include "transcript.h"

/* `label = answer | calls`, the calls being those made since the last step. */
static void step(const char *label, KRef answer) {
    CHECK(kt_pending_exception() == NULL, "a lookup raised\n");
    say(label);
    say(" = ");
    say_value(answer);
    say(" | ");
    say_bytes(call_log, call_log_length);
    say("\n");
    call_log_length = 0;
}

static KRef yes_no(kt_boolean value) { return kt_box_boolean(value); }

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef a1 = key(1, 7, "a1");
    KRef b = key(2, 7, "b");
    KRef c = key(3, 8, "c");
    KRef one = kt_string_utf8("1", 1);
    KRef two = kt_string_utf8("2", 1);
    KRef three = kt_string_utf8("3", 1);

    KRef m = kt_map_new();
    step("put a1", kt_map_put(m, a1, one));
    step("put b", kt_map_put(m, b, two));
    step("put c", kt_map_put(m, c, three));
    step("get a1", kt_map_get(m, a1));
    step("get q", kt_map_get(m, key(1, 7, "q")));
    step("get r", kt_map_get(m, key(9, 7, "r")));
    step("get s", kt_map_get(m, key(9, 99, "s")));
    step("containsKey c", yes_no(kt_map_contains_key(m, c)));
    step("get x", kt_map_get(m, asymmetric(1, "x")));
    step("remove b", kt_map_remove(m, b));
    step("get c", kt_map_get(m, c));

    KRef s = kt_set_new();
    step("add a1", yes_no(kt_set_add(s, a1)));
    step("add c", yes_no(kt_set_add(s, c)));
    step("contains z", yes_no(kt_set_contains(s, asymmetric(1, "z"))));
    step("add d", yes_no(kt_set_add(s, key(1, 7, "d"))));
    step("remove e", yes_no(kt_set_remove(s, key(3, 8, "e"))));

    KRef vm = kt_map_new();
    step("put x", kt_map_put(vm, kt_string_utf8("x", 1), a1));
    step("put y", kt_map_put(vm, kt_string_utf8("y", 1), c));
    step("containsValue f", yes_no(kt_map_contains_value(vm, key(3, 0, "f"))));
    step("containsValue a1", yes_no(kt_map_contains_value(vm, a1)));
    step("containsValue g", yes_no(kt_map_contains_value(vm, asymmetric(3, "g"))));

    KRef fresh = kt_map_new();
    step("fresh get a", kt_map_get(fresh, key(1, 7, "a")));
    step("fresh containsKey b", yes_no(kt_map_contains_key(fresh, key(1, 7, "b"))));
    step("fresh getOrDefault o", kt_map_get_or_default(fresh, key(1, 7, "o"), kt_box_int(5)));
    step("fresh remove c", kt_map_remove(fresh, key(1, 7, "c")));
    KRef fresh_set = kt_set_new();
    step("fresh set contains f", yes_no(kt_set_contains(fresh_set, key(1, 7, "f"))));
    step("fresh set remove g", yes_no(kt_set_remove(fresh_set, key(1, 7, "g"))));
    step("put j", kt_map_put(fresh, key(1, 7, "j"), one));
    kt_map_clear(fresh);
    step("cleared get k", kt_map_get(fresh, key(1, 7, "k")));

    kt_sys_write(1, "OK\n", 3);
}
