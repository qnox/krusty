/* What a list IS, and what it equals. The read-only list `listOf` answers is a `List`, a
   `Collection`, an `Iterable` and `RandomAccess`, and not a `MutableList`, `MutableCollection` or
   `MutableIterable`; the growable one is `kotlin.collections.ArrayList` and all seven. A list
   equals any `List` with equal elements in order, a list class of the program's own included, and
   nothing that is merely `Iterable`. The lists used to name only a `List` marker, and `equals`
   accepted only the runtime's two list classes, so an equal program list compared unequal.

   The driver prints what each list is and what each comparison answers, with the calls made into
   the program list, and the harness compares the lines with what `list_identity.kt` answers under
   the reference kotlinc. The read-only list is Kotlin/Native's `Array.asList`, which is no
   `MutableList` and has no simple name, where the JVM's `java.util.Arrays$ArrayList` is both -- a
   declared divergence. Its `equals` walks a program list with `iterator()`, `hasNext` and `next`,
   which is neither platform's order -- a declared known gap (see the test). */
#include "program_collections.h"
#include "transcript.h"

static KRef list_of(KRef a, KRef b) {
    KRef elements = kt_array_new(&kt_type_array, 2);
    ((KRef *)((KArray *)elements + 1))[0] = a;
    ((KRef *)((KArray *)elements + 1))[1] = b;
    return kt_list_of(elements);
}

/* `label answer`, and the calls made into the program list since the log was emptied when
   `calls` says so; the log is left for the driver's own check. */
static void say_equal(const char *label, kt_boolean answer) {
    say(label);
    say(" ");
    say_bool(answer);
    say(" | ");
    say_bytes(call_log, call_log_length);
    say("\n");
    call_log_length = 0;
}

void kt_program_entry(void) {
    PROGRAM_BEGIN();
    KRef read_only = list_of(tag(1, ""), tag(2, ""));
    KRef growable = kt_mutable_list_new();
    kt_mutable_list_add(growable, tag(1, ""));
    kt_mutable_list_add(growable, tag(2, ""));

    const KType *const everything[] = {
        &kt_type_list_interface,          &kt_type_collection_interface,
        &kt_type_iterable_interface,      &kt_type_random_access_interface,
        &kt_type_mutable_list_interface,  &kt_type_mutable_collection_interface,
        &kt_type_mutable_iterable_interface};
    say("mutableListOf:");
    for (unsigned at = 0; at < 7; at++) {
        say(" ");
        say_bool(kt_is_instance(growable, everything[at]));
    }
    say("\nlistOf:");
    for (unsigned at = 0; at < 7; at++) {
        say(" ");
        say_bool(kt_is_instance(read_only, everything[at]));
    }
    say("\n");
    say("mutableListOf::class.simpleName ");
    say_text(kt_class_simple_name(kt_class_of(growable)));
    say("\n");
    say("listOf::class.simpleName ");
    say_value(kt_class_simple_name(kt_class_of(read_only)));
    say("\n");

    KRef one = tag(1, "");
    KRef two = tag(2, "");
    KRef three = tag(3, "");
    KRef p12[] = {one, two};
    KRef p123[] = {one, two, three};
    KRef p1[] = {one};
    KRef p13[] = {one, three};
    call_log_length = 0;
    say_equal("listOf(1, 2) == P(1, 2)", kt_equals(read_only, seq_of(&seq_list_type, p12, 2)));
    say_equal("listOf(1, 2) == P(1, 2, 3)",
              kt_equals(read_only, seq_of(&seq_list_type, p123, 3)));
    say_equal("listOf(1, 2) == P(1)", kt_equals(read_only, seq_of(&seq_list_type, p1, 1)));
    say_equal("listOf(1, 2) == P(1, 3)", kt_equals(read_only, seq_of(&seq_list_type, p13, 2)));
    say_equal("listOf(1, 2) == Q(1, 2)", kt_equals(read_only, seq_of(&seq_type, p12, 2)));
    say_equal("listOf() == P()", kt_equals(kt_list_empty(), seq_of(&seq_list_type, p12, 0)));
    say_equal("mutableListOf(1, 2) == P(1, 2)",
              kt_equals(growable, seq_of(&seq_list_type, p12, 2)));
    say_equal("mutableListOf(1, 2) == P(1, 2, 3)",
              kt_equals(growable, seq_of(&seq_list_type, p123, 3)));
    say_equal("mutableListOf(1, 2) == P(1)", kt_equals(growable, seq_of(&seq_list_type, p1, 1)));
    say_equal("mutableListOf(1, 2) == P(1, 3)",
              kt_equals(growable, seq_of(&seq_list_type, p13, 2)));
    say_equal("listOf(1, 2) == mutableListOf(1, 2)", kt_equals(read_only, growable));
    say_equal("mutableListOf(1, 2) == listOf(1, 2)", kt_equals(growable, read_only));

    /* A program list whose `hasNext` or `next` throws: the comparison stops there, with that
       exception pending, whichever placeholder the throwing call answered. */
    for (int placeholder = 0; placeholder <= 1; placeholder++) {
        KRef list = seq_of(&seq_list_type, p12, 2);
        (void)kt_equals(read_only, seq_throwing(list, "h", 1, placeholder));
        CHECK(threw_last() && log_is("iterator hasNext next eq(1) hasNext "),
              "a throwing hasNext ended the comparison\n");
        list = seq_of(&seq_list_type, p12, 2);
        (void)kt_equals(read_only, seq_throwing(list, "n", 0, placeholder));
        CHECK(threw_last() && log_is("iterator hasNext next "),
              "a throwing next ended the comparison\n");
        list = seq_of(&seq_list_type, p12, 2);
        (void)kt_equals(read_only, seq_throwing(list, "i", 0, placeholder));
        CHECK(threw_last() && log_is("iterator "), "a throwing iterator ended the comparison\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
