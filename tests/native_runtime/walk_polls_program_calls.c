/* Every runtime walk over an `Iterable` the PROGRAM declares stops at the first of the program's
   calls that throws — its `iterator()`, its iterator's `hasNext()` or `next()`, or a lambda or
   element member the walk calls — makes no further call into the program, and leaves that very
   exception pending. The walks used to consume the `iterator()` and `hasNext()` answers before
   looking: a throwing `iterator()` left them asking a NULL or a stray object for its elements, a
   throwing `hasNext()` that answered `true` went on to `next()`, and one that answered `false`
   ended the walk as if complete, where `first { }` then replaced the exception with its own.

   The driver prints, for each walk and each throwing call, whether the walk threw and the calls it
   made into the program, and the harness compares the lines with what
   `walk_polls_program_calls.kt` answers under the reference kotlinc. The driver itself checks that
   the exception pending is the very one the program threw, and runs each case again with the
   throwing call answering the other placeholder polarity, which must change nothing. */
#include "program_collections.h"
#include "transcript.h"

static KRef f_logged(void) {
    log_text("f ");
    return NULL;
}

static KRef pred_false(KRef self, KRef element) {
    (void)self;
    (void)element;
    (void)f_logged();
    return kt_box_boolean(0);
}

static KRef pred_true(KRef self, KRef element) {
    (void)self;
    (void)element;
    (void)f_logged();
    return kt_box_boolean(1);
}

static KRef identity(KRef self, KRef element) {
    (void)self;
    (void)f_logged();
    return element;
}

static KRef keep_accumulator(KRef self, KRef accumulator, KRef element) {
    (void)self;
    (void)element;
    (void)f_logged();
    return accumulator;
}

static KRef compare_equal(KRef self, KRef a, KRef b) {
    (void)self;
    (void)a;
    (void)b;
    (void)f_logged();
    return kt_box_int(0);
}

static KRef select_one(KRef self, KRef element) {
    (void)self;
    (void)element;
    (void)f_logged();
    return kt_box_int(1);
}

FUNCTION_VALUE(f_false, pred_false)
FUNCTION_VALUE(f_true, pred_true)
FUNCTION_VALUE(f_identity, identity)
FUNCTION_VALUE(f_fold, keep_accumulator)
FUNCTION_VALUE(f_compare, compare_equal)
FUNCTION_VALUE(f_select, select_one)

static void w_map(KRef s) { (void)kt_iterable_map(s, f_identity); }
static void w_for_each(KRef s) { kt_iterable_for_each(s, f_identity); }
static void w_any(KRef s) { (void)kt_iterable_any(s, f_false); }
static void w_all(KRef s) { (void)kt_iterable_all(s, f_true); }
static void w_none(KRef s) { (void)kt_iterable_none(s, f_false); }
static void w_count(KRef s) { (void)kt_iterable_count(s); }
static void w_count_if(KRef s) { (void)kt_iterable_count_matching(s, f_true); }
static void w_filter(KRef s) { (void)kt_iterable_filter(s, f_true); }
static void w_first_or_null(KRef s) { (void)kt_iterable_first_or_null(s, f_false); }
static void w_first(KRef s) { (void)kt_iterable_first_matching(s, f_false); }
static void w_last(KRef s) { (void)kt_iterable_last_matching(s, f_false); }
static void w_fold(KRef s) { (void)kt_iterable_fold(s, kt_box_int(0), f_fold); }
static void w_for_each_indexed(KRef s) { kt_iterable_for_each_indexed(s, f_fold); }
static void w_to_list(KRef s) { (void)kt_iterable_to_list(s); }
static void w_reversed(KRef s) { (void)kt_iterable_reversed(s); }
static void w_sorted_with(KRef s) { (void)kt_iterable_sorted_with(s, f_compare); }
static void w_index_of(KRef s) { (void)kt_iterable_index_of(s, tag(9, "")); }
static void w_join(KRef s) { (void)kt_iterable_join_to_string(s); }
static void w_plus(KRef s) { (void)kt_iterable_plus_element(s, tag(5, "")); }
static void w_sum_of(KRef s) { (void)kt_iterable_sum_of_int(s, f_select); }
static void w_with_index(KRef s) { (void)kt_iterable_to_list(kt_iterable_with_index(s)); }
static void w_add_all(KRef s) { kt_mutable_list_add_all(kt_mutable_list_new(), s); }

typedef struct Walk {
    const char *name;
    void (*run)(KRef seq);
} Walk;

static const Walk walks[] = {
    {"map", w_map},
    {"forEach", w_for_each},
    {"any", w_any},
    {"all", w_all},
    {"none", w_none},
    {"count", w_count},
    {"count { }", w_count_if},
    {"filter", w_filter},
    {"firstOrNull", w_first_or_null},
    {"first", w_first},
    {"last", w_last},
    {"fold", w_fold},
    {"forEachIndexed", w_for_each_indexed},
    {"toList", w_to_list},
    {"reversed", w_reversed},
    {"sortedWith", w_sorted_with},
    {"indexOf", w_index_of},
    {"joinToString", w_join},
    {"plus", w_plus},
    {"sumOf", w_sum_of},
    {"withIndex", w_with_index},
    {"addAll", w_add_all},
};

/* The calls the first polarity's run made, which the second must repeat exactly. */
static char first_calls[sizeof(call_log) + 1];

void kt_program_entry(void) {
    PROGRAM_BEGIN();
    KRef elements[] = {tag(0, ""), tag(1, "")};
    static const char throwing[] = {'i', 'h', 'n'};
    for (unsigned w = 0; w < sizeof(walks) / sizeof(walks[0]); w++) {
        for (unsigned t = 0; t < 3; t++) {
            char calls[2] = {throwing[t], 0};
            for (int placeholder = 0; placeholder <= 1; placeholder++) {
                KRef seq = seq_throwing(seq_of(&seq_type, elements, 2), calls, 1, placeholder);
                call_log_length = 0;
                walks[w].run(seq);
                CHECK(threw_last(), "a walk did not stop at the program's throwing call\n");
                if (placeholder == 0) {
                    say(walks[w].name);
                    say(" ");
                    say(calls);
                    say(" threw | ");
                    say_bytes(call_log, call_log_length);
                    say("\n");
                    for (kt_int at = 0; at < call_log_length; at++) {
                        first_calls[at] = call_log[at];
                    }
                    first_calls[call_log_length] = 0;
                } else {
                    CHECK(log_is(first_calls), "a walk's calls depend on a placeholder\n");
                }
            }
        }
    }

    /* `none()` and `any()` ask `hasNext` once, before the throwing one. */
    for (unsigned t = 1; t < 3; t++) {
        char calls[2] = {throwing[t], 0};
        KRef seq = seq_throwing(seq_of(&seq_type, elements, 2), calls, 1, 0);
        call_log_length = 0;
        say("none() ");
        say(calls);
        say(" ");
        say_bool(kt_iterable_is_empty(seq));
        say(" | ");
        say_bytes(call_log, call_log_length);
        say("\n");
        call_log_length = 0;
        say("any() ");
        say(calls);
        say(" ");
        say_bool(kt_iterable_is_not_empty(seq));
        say(" | ");
        say_bytes(call_log, call_log_length);
        say("\n");
        CHECK(kt_pending_exception() == NULL, "none() or any() raised\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
