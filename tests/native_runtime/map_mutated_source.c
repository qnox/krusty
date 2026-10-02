/* `map` and `forEach` over a `MutableList` the lambda changes structurally stop the way Kotlin's
   do. Kotlin's `map` sizes its result from the list but walks the list's iterator, asking
   `hasNext()` before every element, and `next()` after a structural change throws
   `ConcurrentModificationException`. The runtime's `map` instead took the size first and called
   `next()` exactly that many times with no final `hasNext()`, so a transform that appended to a
   one-element list answered a one-element result.

   The iterator answers `hasNext()` as Kotlin/Native's `ArrayList` does, `cursor < size`, so a walk
   whose last element removes an element ends cleanly; the JVM's answers `cursor != size` and
   throws there. The driver prints each answer and the list afterwards, and the harness compares the
   lines with what `map_mutated_source.kt` answers under the reference kotlinc, those two lines
   being declared divergences. */
#include "collections_later_tiers.h"
#include "transcript.h"

static KRef source;

static kt_int n_of(KRef value) { return kt_unbox_int(value); }

static KRef append_next(KRef self, KRef element) {
    (void)self;
    kt_mutable_list_add(source, kt_box_int(n_of(element) + 1));
    return kt_box_int(n_of(element) * 10);
}

static KRef append_at_two(KRef self, KRef element) {
    (void)self;
    if (n_of(element) == 2) {
        kt_mutable_list_add(source, kt_box_int(3));
    }
    return kt_box_int(n_of(element) * 10);
}

static KRef remove_first_at_one(KRef self, KRef element) {
    (void)self;
    if (n_of(element) == 1) {
        (void)kt_mutable_list_remove_at(source, 0);
    }
    return kt_box_int(n_of(element) * 10);
}

static KRef remove_last_at_two(KRef self, KRef element) {
    (void)self;
    if (n_of(element) == 2) {
        (void)kt_mutable_list_remove_at(source, 1);
    }
    return kt_box_int(n_of(element) * 10);
}

static KRef set_second_at_one(KRef self, KRef element) {
    (void)self;
    if (n_of(element) == 1) {
        (void)kt_mutable_list_set(source, 1, kt_box_int(5));
    }
    return kt_box_int(n_of(element) * 10);
}

static KRef times_ten(KRef self, KRef element) {
    (void)self;
    return kt_box_int(n_of(element) * 10);
}

FUNCTION_VALUE(f_append_next, append_next)
FUNCTION_VALUE(f_append_at_two, append_at_two)
FUNCTION_VALUE(f_remove_first_at_one, remove_first_at_one)
FUNCTION_VALUE(f_remove_last_at_two, remove_last_at_two)
FUNCTION_VALUE(f_set_second_at_one, set_second_at_one)
FUNCTION_VALUE(f_times_ten, times_ten)

/* A fresh `source` holding 1 up to `count`. */
static void fill(kt_int count) {
    source = kt_mutable_list_new();
    for (kt_int n = 1; n <= count; n++) {
        kt_mutable_list_add(source, kt_box_int(n));
    }
}

/* `label: answer source`, the answer being what the walk returned, or what it threw. */
static void show(const char *label, KRef answer, kt_boolean unit) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    say(label);
    say(": ");
    if (thrown != NULL) {
        say_thrown(thrown);
    } else if (unit) {
        say("kotlin.Unit");
    } else {
        say_value(answer);
    }
    say(" ");
    say_value(source);
    say("\n");
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_gc_add_global_root((void **)&source);

    fill(1);
    show("map appending to [1]", kt_iterable_map(source, f_append_next), 0);
    fill(2);
    show("map appending at the last of [1, 2]", kt_iterable_map(source, f_append_at_two), 0);
    fill(2);
    show("map removing the first at the first of [1, 2]",
         kt_iterable_map(source, f_remove_first_at_one), 0);
    fill(2);
    show("map removing the last at the last of [1, 2]",
         kt_iterable_map(source, f_remove_last_at_two), 0);
    fill(2);
    show("map setting the second at the first of [1, 2]",
         kt_iterable_map(source, f_set_second_at_one), 0);
    fill(2);
    kt_iterable_for_each(source, f_remove_last_at_two);
    show("forEach removing the last at the last of [1, 2]", NULL, 1);
    fill(2);
    show("map of an unchanged [1, 2]", kt_iterable_map(source, f_times_ten), 0);
    kt_sys_write(1, "OK\n", 3);
}
