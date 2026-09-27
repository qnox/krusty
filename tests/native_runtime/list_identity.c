/* What a list IS, and what it equals. The read-only list `listOf` answers is a `List`, a
   `Collection`, an `Iterable` and `RandomAccess`, and not a `MutableList`, `MutableCollection` or
   `MutableIterable`; the growable one is `kotlin.collections.ArrayList` and all seven. A list
   equals any `List` with equal elements in order, a list class of the program's own included, and
   nothing that is merely `Iterable`. The lists used to name only a `List` marker, and `equals`
   accepted only the runtime's two list classes, so an equal program list compared unequal.

   The expected answers are Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       class P(private vararg val xs: Int) : List<Int> { ... every member over `xs` ... }
       class Q(private vararg val xs: Int) : Iterable<Int> {
           override fun iterator() = xs.iterator()
       }
       fun main() {
           val r: Any = listOf(1, 2); val m: Any = mutableListOf(1, 2)
           for (x in listOf(r, m)) println("${x is List<*>} ${x is Collection<*>} " +
               "${x is Iterable<*>} ${x is RandomAccess} ${x is MutableList<*>} " +
               "${x is MutableCollection<*>} ${x is MutableIterable<*>}")
           println("${listOf(1, 2) == P(1, 2)} ${mutableListOf(1, 2) == P(1, 2)} " +
               "${listOf(1, 2) == P(1, 2, 3)} ${listOf(1, 2) == P(1)} ${listOf(1, 2) == P(1, 3)} " +
               "${(listOf(1, 2) as Any) == Q(1, 2)} ${listOf<Int>() == P()}")
           println("${mutableListOf(1, 2)::class.simpleName}")
       }

   which prints `true true true true true true true` twice, `true true false false false false
   true` and `ArrayList`. The one difference is deliberate: the JVM's `listOf` answers
   `java.util.Arrays$ArrayList`, which is a `MutableList` to an `is`, where Kotlin/Native's answers
   an anonymous read-only `AbstractList` that is not; this runtime is the native one, so its
   read-only list is not a `MutableList`, and has no class name to publish (`docs/SPEC.md`). The
   order of calls into a program list is the JVM's `AbstractList.equals`: `hasNext`, `next`, then
   the element's `equals`, and a last `hasNext`. */
#include "program_collections.h"

static KRef list_of(KRef a, KRef b) {
    KRef elements = kt_array_new(&kt_type_array, 2);
    ((KRef *)((KArray *)elements + 1))[0] = a;
    ((KRef *)((KArray *)elements + 1))[1] = b;
    return kt_list_of(elements);
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
    for (unsigned at = 0; at < 7; at++) {
        CHECK(kt_is_instance(growable, everything[at]), "a mutable list is not every interface\n");
        CHECK(kt_is_instance(read_only, everything[at]) == (at < 4),
              "a read-only list's interfaces\n");
    }
    CHECK(text_is(kt_class_simple_name(kt_class_of(growable)), "ArrayList", 9),
          "mutableListOf(1, 2)::class.simpleName\n");
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
    CHECK(kt_equals(read_only, seq_of(&seq_list_type, p12, 2)) &&
              log_is("iterator hasNext next eq(1) hasNext next eq(2) hasNext "),
          "listOf(1, 2) == P(1, 2)\n");
    CHECK(kt_equals(growable, seq_of(&seq_list_type, p12, 2)) &&
              log_is("iterator hasNext next eq(1) hasNext next eq(2) hasNext "),
          "mutableListOf(1, 2) == P(1, 2)\n");
    CHECK(!kt_equals(read_only, seq_of(&seq_list_type, p123, 3)) &&
              log_is("iterator hasNext next eq(1) hasNext next eq(2) hasNext "),
          "listOf(1, 2) == P(1, 2, 3)\n");
    CHECK(!kt_equals(read_only, seq_of(&seq_list_type, p1, 1)) &&
              log_is("iterator hasNext next eq(1) hasNext "),
          "listOf(1, 2) == P(1)\n");
    CHECK(!kt_equals(read_only, seq_of(&seq_list_type, p13, 2)) &&
              log_is("iterator hasNext next eq(1) hasNext next eq(2) "),
          "listOf(1, 2) == P(1, 3)\n");
    CHECK(!kt_equals(read_only, seq_of(&seq_type, p12, 2)) && log_is(""),
          "listOf(1, 2) == Q(1, 2)\n");
    CHECK(kt_equals(kt_list_empty(), seq_of(&seq_list_type, p12, 0)) &&
              log_is("iterator hasNext "),
          "listOf<Int>() == P()\n");
    CHECK(kt_equals(read_only, growable) && kt_equals(growable, read_only),
          "listOf(1, 2) and mutableListOf(1, 2) compare unequal\n");
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
