/* What a list IS, and what it equals. The read-only list `listOf` answers is a `List`, a
   `Collection`, an `Iterable` and `RandomAccess`, and not a `MutableList`, `MutableCollection` or
   `MutableIterable`; the growable one is `kotlin.collections.ArrayList` and all seven. A list
   equals any `List` with equal elements in order, a list class of the program's own included, and
   nothing that is merely `Iterable`. The lists used to name only a `List` marker, and `equals`
   accepted only the runtime's two list classes, so an equal program list compared unequal.

   The driver prints what each list is and what each comparison answers, and the harness compares
   the lines with what `list_identity.kt` answers under the reference kotlinc, the calls made into
   the program list included where the receiver is the growable list. Two things differ from the
   JVM on purpose and are pinned here instead. The JVM's `listOf` answers `java.util.Arrays$ArrayList`,
   which is a `MutableList` to an `is`, where Kotlin/Native's answers an anonymous read-only
   `AbstractList` that is not; this runtime is the native one, so its read-only list is not a
   `MutableList`, and has no class name to publish (`docs/SPEC.md`). And the runtime reaches a
   program's collection only through its `iterator()`, so a read-only list's `equals` walks it with
   `iterator()` where the JVM's `AbstractList.equals` asks for `listIterator()`, and an empty one
   walks it where Kotlin's `EmptyList.equals` asks `isEmpty()`. */
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
static void say_equal(const char *label, kt_boolean answer, kt_boolean calls) {
    say(label);
    say(" ");
    say_bool(answer);
    if (calls) {
        say(" | ");
        say_bytes(call_log, call_log_length);
    }
    say("\n");
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
    for (unsigned at = 0; at < 4; at++) {
        say(" ");
        say_bool(kt_is_instance(read_only, everything[at]));
    }
    say("\n");
    /* The native read-only list is none of the mutable interfaces. */
    for (unsigned at = 4; at < 7; at++) {
        CHECK(!kt_is_instance(read_only, everything[at]), "a read-only list is mutable\n");
    }
    say("mutableListOf::class.simpleName ");
    say_text(kt_class_simple_name(kt_class_of(growable)));
    say("\n");
    CHECK(kt_class_simple_name(kt_class_of(read_only)) == NULL,
          "the read-only list published a class name\n");

    KRef one = tag(1, "");
    KRef two = tag(2, "");
    KRef three = tag(3, "");
    KRef p12[] = {one, two};
    KRef p123[] = {one, two, three};
    KRef p1[] = {one};
    KRef p13[] = {one, three};
    call_log_length = 0;
    say_equal("listOf(1, 2) == P(1, 2)", kt_equals(read_only, seq_of(&seq_list_type, p12, 2)), 0);
    CHECK(log_is("iterator hasNext next eq(1) hasNext next eq(2) hasNext "),
          "listOf(1, 2) == P(1, 2)\n");
    say_equal("listOf(1, 2) == P(1, 2, 3)",
              kt_equals(read_only, seq_of(&seq_list_type, p123, 3)), 0);
    CHECK(log_is("iterator hasNext next eq(1) hasNext next eq(2) hasNext "),
          "listOf(1, 2) == P(1, 2, 3)\n");
    say_equal("listOf(1, 2) == P(1)", kt_equals(read_only, seq_of(&seq_list_type, p1, 1)), 0);
    CHECK(log_is("iterator hasNext next eq(1) hasNext "), "listOf(1, 2) == P(1)\n");
    say_equal("listOf(1, 2) == P(1, 3)", kt_equals(read_only, seq_of(&seq_list_type, p13, 2)), 0);
    CHECK(log_is("iterator hasNext next eq(1) hasNext next eq(2) "), "listOf(1, 2) == P(1, 3)\n");
    say_equal("listOf(1, 2) == Q(1, 2)", kt_equals(read_only, seq_of(&seq_type, p12, 2)), 1);
    CHECK(log_is(""), "listOf(1, 2) == Q(1, 2)\n");
    say_equal("listOf() == P()", kt_equals(kt_list_empty(), seq_of(&seq_list_type, p12, 0)), 0);
    CHECK(log_is("iterator hasNext "), "listOf<Int>() == P()\n");
    say_equal("mutableListOf(1, 2) == P(1, 2)",
              kt_equals(growable, seq_of(&seq_list_type, p12, 2)), 1);
    CHECK(log_is("iterator hasNext next eq(1) hasNext next eq(2) hasNext "),
          "mutableListOf(1, 2) == P(1, 2)\n");
    say_equal("mutableListOf(1, 2) == P(1, 2, 3)",
              kt_equals(growable, seq_of(&seq_list_type, p123, 3)), 1);
    call_log_length = 0;
    say_equal("mutableListOf(1, 2) == P(1)", kt_equals(growable, seq_of(&seq_list_type, p1, 1)),
              1);
    call_log_length = 0;
    say_equal("mutableListOf(1, 2) == P(1, 3)",
              kt_equals(growable, seq_of(&seq_list_type, p13, 2)), 1);
    call_log_length = 0;
    say_equal("listOf(1, 2) == mutableListOf(1, 2)", kt_equals(read_only, growable), 0);
    say_equal("mutableListOf(1, 2) == listOf(1, 2)", kt_equals(growable, read_only), 0);
    call_log_length = 0;

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
