/* `sb.map { … }` over a `StringBuilder` walks it the way Kotlin's `CharSequence.iterator()` does,
   asking its CURRENT length before every step, so a transform that grows or shrinks the builder
   changes how many elements the walk yields; the map used to size its result from the length
   before the walk. And a list that holds itself renders that element as `(this Collection)`, as
   Kotlin's `AbstractCollection.toString` does, where it used to recurse without end.

   The expected answers are Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       fun main() {
           val sb = StringBuilder("ab")
           val r = sb.map { c -> if (sb.length < 4) sb.append('z'); c }
           println("$r $sb")
           val sb2 = StringBuilder("abcd")
           val r2 = sb2.map { c -> if (sb2.length > 2) sb2.setLength(sb2.length - 1); c }
           println("$r2 $sb2")
           val l = mutableListOf<Any>(1, 2); l.add(l); l.add(3)
           println(l)
           val m = mutableListOf<Any>(); m.add(m); println(m)
       }

   which prints `[a, b, z, z] abzz`, `[a, b] ab`, `[1, 2, (this Collection), 3]` and
   `[(this Collection)]`. */
#include "collections_later_tiers.h"

static KRef builder;

static KRef grow(KRef self, KRef element) {
    (void)self;
    if (kt_string_length(builder) < 4) {
        (void)kt_string_builder_append(builder, kt_box_char('z'));
    }
    return element;
}

static KRef shrink(KRef self, KRef element) {
    (void)self;
    kt_int length = kt_string_length(builder);
    if (length > 2) {
        kt_string_builder_set_length(builder, length - 1);
    }
    return element;
}

FUNCTION_VALUE(f_grow, grow)
FUNCTION_VALUE(f_shrink, shrink)

#define RENDERS(value, literal) text_is(kt_to_string(value), literal, sizeof(literal) - 1)

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_gc_add_global_root((void **)&builder);

    builder = kt_string_builder_with_text(kt_string_utf8("ab", 2));
    KRef grown = kt_iterable_map(builder, f_grow);
    CHECK(RENDERS(grown, "[a, b, z, z]") && text_is(builder, "abzz", 4),
          "a map that grows its builder\n");
    builder = kt_string_builder_with_text(kt_string_utf8("abcd", 4));
    KRef shrunk = kt_iterable_map(builder, f_shrink);
    CHECK(RENDERS(shrunk, "[a, b]") && text_is(builder, "ab", 2),
          "a map that shrinks its builder\n");

    KRef list = kt_mutable_list_new();
    kt_mutable_list_add(list, kt_box_int(1));
    kt_mutable_list_add(list, kt_box_int(2));
    kt_mutable_list_add(list, list);
    kt_mutable_list_add(list, kt_box_int(3));
    CHECK(RENDERS(list, "[1, 2, (this Collection), 3]"), "a list holding itself\n");
    KRef alone = kt_mutable_list_new();
    kt_mutable_list_add(alone, alone);
    CHECK(RENDERS(alone, "[(this Collection)]"), "a list holding only itself\n");
    CHECK(kt_pending_exception() == NULL, "mapping or rendering raised\n");
    kt_sys_write(1, "OK\n", 3);
}
