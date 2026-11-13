/* krusty native runtime: `kotlin.text.Appendable`'s range `append`, realized on the runtime's own
   `StringBuilder`.

   A translation unit of its own, beside `krusty_rt.c`, which holds the builder's storage and its
   one-operand appends and is already as long as one file should be. Everything here goes through
   the builder's public entry points, and the bounds message through `krusty_internal.h`. */
#include "krusty_internal.h"

/* `sb.append(value, startIndex, endIndex)`: the units of `value ?: "null"` from `startIndex` until
   `endIndex`, answering the RECEIVER. The bounds are checked as Kotlin/Native's builder checks
   them, against the value's length before anything is appended: a bound outside it raises
   `IndexOutOfBoundsException`, a start after the end `IllegalArgumentException`, each worded as its
   `AbstractList.checkBoundsIndexes` words it. The bounds arrive boxed, as every operand of a
   runtime member does.

   A `String` or a builder is cut by UTF-16 unit, a split surrogate pair answering its half, and the
   append joins two halves that meet. A `CharSequence` the PROGRAM implements has no bytes to cut,
   so it is read unit by unit through its own `length` and `get`, only within the range, as Kotlin's
   own builder reads it; a throw from either stops the append there. */
KRef kt_string_builder_append_range(KRef self, KRef value, KRef start_index, KRef end_index) {
    kt_int start = kt_unbox_int(start_index);
    kt_int end = kt_unbox_int(end_index);
    /* `text` stays live in this local across every allocation below. */
    KRef text = value != NULL ? value : kt_string_utf8("null", 4);
    kt_boolean walked = text->header.type->walk_length != NULL;
    kt_int length = walked ? text->header.type->walk_length(text) : kt_string_length(text);
    if (kt_pending_exception() != NULL) {
        return self;
    }
    if (start < 0 || end > length) {
        KRef message =
            kt_bounds_text("startIndex: ", start, ", endIndex: ", end, ", size: ", length);
        kt_throw(kt_throwable_new(&kt_type_index_out_of_bounds_exception, message));
        return self;
    }
    if (start > end) {
        KRef message = kt_bounds_text("startIndex: ", start, " > endIndex: ", end, NULL, 0);
        kt_throw(kt_throwable_new(&kt_type_illegal_argument_exception, message));
        return self;
    }
    if (!walked) {
        return kt_string_builder_append(self, kt_string_substring(text, start, end));
    }
    for (kt_int index = start; index < end; index++) {
        kt_char unit = text->header.type->walk_char_at(text, index);
        if (kt_pending_exception() != NULL) {
            return self;
        }
        self = kt_string_builder_append(self, kt_box_char(unit));
    }
    return self;
}
