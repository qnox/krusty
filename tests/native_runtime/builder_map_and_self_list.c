/* `sb.map { … }` over a `StringBuilder` walks it the way Kotlin's `CharSequence.iterator()` does,
   asking its CURRENT length before every step, so a transform that grows or shrinks the builder
   changes how many elements the walk yields; the map used to size its result from the length
   before the walk. And a list that holds itself renders that element as `(this Collection)`, as
   Kotlin's `AbstractCollection.toString` does, where it used to recurse without end.

   The driver prints each result, rendered by the runtime, and the harness compares the lines with
   what `builder_map_and_self_list.kt` answers under the reference kotlinc. */
#include "driver_exceptions.h"
#include "transcript.h"

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

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_gc_add_global_root((void **)&builder);

    builder = kt_string_builder_with_text(kt_string_utf8("ab", 2));
    say_value(kt_iterable_map(builder, f_grow));
    say(" ");
    say_value(builder);
    say("\n");
    builder = kt_string_builder_with_text(kt_string_utf8("abcd", 4));
    say_value(kt_iterable_map(builder, f_shrink));
    say(" ");
    say_value(builder);
    say("\n");

    KRef list = kt_mutable_list_new();
    kt_mutable_list_add(list, kt_box_int(1));
    kt_mutable_list_add(list, kt_box_int(2));
    kt_mutable_list_add(list, list);
    kt_mutable_list_add(list, kt_box_int(3));
    say_value(list);
    say("\n");
    KRef alone = kt_mutable_list_new();
    kt_mutable_list_add(alone, alone);
    say_value(alone);
    say("\n");
    CHECK(kt_pending_exception() == NULL, "mapping or rendering raised\n");
    kt_sys_write(1, "OK\n", 3);
}
