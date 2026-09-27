/* The list, walk and array entry points answer what Kotlin answers on the ordinary path. The driver
   prints each answer, rendered by the runtime, and the harness compares the lines with what
   `list_api_answers.kt` answers under the reference kotlinc, where `T` is the program class the
   driver's `Tag` (`program_collections.h`) stands in for. */
#include "program_collections.h"
#include "transcript.h"

#define ELEMENT(array, at) (((KRef *)((KArray *)(array) + 1))[at])

static KRef array_of(KRef *elements, kt_int count) {
    KRef array = kt_array_new(&kt_type_array, count);
    for (kt_int at = 0; at < count; at++) {
        ELEMENT(array, at) = elements[at];
    }
    return array;
}

static KRef ints(kt_int a, kt_int b, kt_int c) {
    KRef elements[] = {kt_box_int(a), kt_box_int(b), kt_box_int(c)};
    return kt_list_of(array_of(elements, 3));
}

static kt_int n_of(KRef value) { return kt_unbox_int(value); }

static KRef times_two(KRef self, KRef x) { (void)self; return kt_box_int(n_of(x) * 2); }
static KRef above_one(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) > 1); }
static KRef above_two(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) > 2); }
static KRef above_zero(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) > 0); }
static KRef above_five(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) > 5); }
static KRef below_three(KRef self, KRef x) { (void)self; return kt_box_boolean(n_of(x) < 3); }
static KRef plus(KRef self, KRef a, KRef b) { (void)self; return kt_box_int(n_of(a) + n_of(b)); }
static KRef ascending(KRef self, KRef a, KRef b) {
    (void)self;
    return kt_box_int(n_of(a) - n_of(b));
}
static KRef identity(KRef self, KRef x) { (void)self; return x; }
static KRef as_long(KRef self, KRef x) { (void)self; return kt_box_long(n_of(x) * 3000000000LL); }
static KRef halved(KRef self, KRef x) { (void)self; return kt_box_double(n_of(x) / 2.0); }

static KRef tags_descending(KRef self, KRef a, KRef b) {
    (void)self;
    return kt_box_int(((const Tag *)b)->n - ((const Tag *)a)->n);
}

static char seen[32];
static kt_int seen_length;

static KRef record(KRef self, KRef index, KRef value) {
    (void)self;
    seen[seen_length++] = (char)('0' + n_of(index));
    seen[seen_length++] = ':';
    seen[seen_length++] = (char)('0' + n_of(value));
    seen[seen_length++] = ',';
    return NULL;
}

FUNCTION_VALUE(f_times_two, times_two)
FUNCTION_VALUE(f_above_one, above_one)
FUNCTION_VALUE(f_above_two, above_two)
FUNCTION_VALUE(f_above_zero, above_zero)
FUNCTION_VALUE(f_above_five, above_five)
FUNCTION_VALUE(f_below_three, below_three)
FUNCTION_VALUE(f_plus, plus)
FUNCTION_VALUE(f_ascending, ascending)
FUNCTION_VALUE(f_identity, identity)
FUNCTION_VALUE(f_as_long, as_long)
FUNCTION_VALUE(f_halved, halved)
FUNCTION_VALUE(f_tags_descending, tags_descending)
FUNCTION_VALUE(f_record, record)

static void sp(void) { say(" "); }

/* The end of a line. The call log `Tag` writes is not this driver's subject; it is emptied so
   that rendering many tags cannot fill it. */
static void nl(void) {
    say("\n");
    call_log_length = 0;
}

static void say_int(kt_int value) { say_long(value); }

void kt_program_entry(void) {
    PROGRAM_BEGIN();

    KRef t123[] = {tag(1, ""), tag(2, ""), tag(3, "")};
    KRef xs = kt_list_of(array_of(t123, 3));
    KRef t12[] = {tag(1, ""), tag(2, "")};
    say_value(xs);
    sp();
    say_int(kt_list_size(xs));
    sp();
    say_value(kt_list_get(xs, 1));
    sp();
    say_value(kt_list_first(xs));
    sp();
    say_value(kt_list_last(xs));
    sp();
    say_int(kt_list_index_of(xs, tag(2, "")));
    sp();
    say_int(kt_list_last_index_of(xs, tag(3, "")));
    sp();
    say_bool(kt_list_contains(xs, tag(4, "")));
    sp();
    say_int(kt_hash_code(xs));
    sp();
    say_bool(kt_equals(xs, kt_list_of(array_of(t123, 3))));
    sp();
    say_bool(kt_equals(xs, kt_list_of(array_of(t12, 2))));
    nl();

    KRef t1[] = {tag(1, "")};
    KRef m = kt_mutable_list_of(array_of(t1, 1));
    kt_mutable_list_add(m, tag(2, ""));
    kt_mutable_list_add_at(m, 0, tag(0, ""));
    KRef old = kt_mutable_list_set(m, 1, tag(5, ""));
    KRef removed = kt_mutable_list_remove_at(m, 0);
    say_value(m);
    sp();
    say_value(old);
    sp();
    say_value(removed);
    sp();
    say_bool(kt_mutable_list_remove(m, tag(5, "")));
    sp();
    say_bool(kt_mutable_list_remove(m, tag(9, "")));
    sp();
    say_int(kt_list_size(m));
    nl();

    KRef t34[] = {tag(3, ""), tag(4, "")};
    kt_mutable_list_add_all(m, kt_list_of(array_of(t34, 2)));
    kt_mutable_list_plus_assign(m, tag(6, ""));
    KRef t2346[] = {tag(2, ""), tag(3, ""), tag(4, ""), tag(6, "")};
    say_value(m);
    sp();
    say_bool(kt_equals(m, kt_list_of(array_of(t2346, 4))));
    sp();
    say_int(kt_hash_code(m));
    nl();
    kt_list_sort_with(m, f_tags_descending);
    say_value(m);
    nl();
    kt_mutable_list_clear(m);
    say_value(m);
    sp();
    say_bool(kt_list_is_empty(m));
    nl();

    KRef ns = ints(3, 1, 2);
    say_value(kt_iterable_map(ns, f_times_two));
    sp();
    say_value(kt_iterable_filter(ns, f_above_one));
    sp();
    say_value(kt_iterable_filter_not(ns, f_above_one));
    sp();
    say_bool(kt_iterable_any(ns, f_above_two));
    sp();
    say_bool(kt_iterable_all(ns, f_above_zero));
    sp();
    say_bool(kt_iterable_none(ns, f_above_five));
    sp();
    say_int(kt_iterable_count(ns));
    sp();
    say_int(kt_iterable_count_matching(ns, f_above_one));
    nl();

    say_value(kt_iterable_first_matching(ns, f_below_three));
    sp();
    say_value(kt_iterable_first_or_null(ns, f_above_five));
    sp();
    say_value(kt_iterable_last_matching(ns, f_above_one));
    sp();
    say_value(kt_iterable_fold(ns, kt_box_int(0), f_plus));
    sp();
    say_value(kt_iterable_to_list(ns));
    sp();
    say_value(kt_iterable_reversed(ns));
    sp();
    say_value(kt_iterable_sorted_with(ns, f_ascending));
    nl();

    kt_iterable_for_each_indexed(ns, f_record);
    say_bytes(seen, seen_length);
    sp();
    say_text(kt_iterable_join_to_string(ns));
    sp();
    KRef five_six[] = {kt_box_int(5), kt_box_int(6)};
    say_value(kt_iterable_plus_element(ns, kt_box_int(4)));
    sp();
    say_value(kt_iterable_plus_all(ns, kt_list_of(array_of(five_six, 2))));
    sp();
    say_int(kt_iterable_sum_of_int(ns, f_identity));
    sp();
    say_long(kt_iterable_sum_of_long(ns, f_as_long));
    sp();
    say_value(kt_box_double(kt_iterable_sum_of_double(ns, f_halved)));
    nl();

    say_value(kt_iterable_to_list(kt_iterable_with_index(ns)));
    sp();
    say_bool(kt_iterable_is_empty(ns));
    sp();
    say_bool(kt_iterable_is_not_empty(ns));
    sp();
    say_bool(kt_iterable_is_not_empty(kt_list_empty()));
    nl();

    KRef iv = kt_indexed_value(1, tag(2, ""));
    say_value(iv);
    sp();
    say_int(kt_hash_code(iv));
    sp();
    say_bool(kt_equals(iv, kt_indexed_value(1, tag(2, ""))));
    sp();
    say_bool(kt_equals(iv, kt_indexed_value(2, tag(2, ""))));
    nl();

    KRef ia = kt_array_new(&kt_type_int_array, 3);
    for (kt_int at = 0; at < 3; at++) {
        ((kt_int *)((KArray *)ia + 1))[at] = at + 1;
    }
    say_value(kt_array_to_list(ia));
    sp();
    say_value(kt_array_reversed(ia));
    sp();
    say_text(kt_array_content_to_string(kt_array_reversed_array(ia)));
    sp();
    say_bool(kt_array_is_empty(ia));
    sp();
    say_bool(kt_array_is_not_empty(kt_array_new(&kt_type_int_array, 0)));
    nl();

    KRef ta = array_of(t12, 2);
    KRef t12_again[] = {tag(1, ""), tag(2, "")};
    say_bool(kt_array_content_equals(ta, array_of(t12_again, 2)));
    sp();
    say_int(kt_array_content_hash_code(ta));
    sp();
    say_text(kt_array_content_to_string(ta));
    sp();
    say_value(kt_array_to_list(ta));
    sp();
    KRef one_two[] = {kt_box_int(1), kt_box_int(2)};
    KRef typed = kt_iterable_to_typed_array(kt_list_of(array_of(one_two, 2)));
    say_text(kt_array_content_to_string(typed));
    nl();

    say_value(kt_iterable_map(kt_range_step(kt_int_range(1, 4), 2), f_identity));
    sp();
    say_value(kt_iterable_to_list(kt_int_range_down_to(3, 1)));
    sp();
    say_text(kt_iterable_join_to_string(kt_char_range('a', 'c')));
    sp();
    say_int(kt_iterable_count(kt_int_range(1, 3)));
    sp();
    say_value(kt_iterable_map(kt_string_utf8("h\xC3\xA9llo", 6), f_identity));
    sp();
    say_value(kt_iterable_to_list(kt_string_utf8("ab", 2)));
    nl();

    say_value(kt_iterable_plus_element(kt_list_of(array_of(t1, 1)), tag(2, "")));
    sp();
    KRef with_null[] = {kt_box_int(1), NULL, kt_box_int(3)};
    say_value(kt_list_of(array_of(with_null, 3)));
    nl();

    CHECK(kt_pending_exception() == NULL, "an ordinary call raised\n");
    kt_sys_write(1, "OK\n", 3);
}
