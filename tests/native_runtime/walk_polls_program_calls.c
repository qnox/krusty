/* Every runtime walk over an `Iterable` the PROGRAM declares stops at the first of the program's
   calls that throws — its `iterator()`, its iterator's `hasNext()` or `next()`, or a lambda or
   element member the walk calls — makes no further call into the program, and leaves that very
   exception pending. The walks used to consume the `iterator()` and `hasNext()` answers before
   looking: a throwing `iterator()` left them asking a NULL or a stray object for its elements, a
   throwing `hasNext()` that answered `true` went on to `next()`, and one that answered `false`
   ended the walk as if complete, where `first { }` then replaced the exception with its own.

   Each case runs with the throwing call answering each placeholder polarity. The calls expected
   are Kotlin's, from this program compiled and run with the reference kotlinc 2.4.10 on the JVM
   (the walks below, each over `Seq(listOf(T(0), T(1)), c, 1)`):

       val log = StringBuilder()
       class Boom : RuntimeException()
       class T(val n: Int) {
           override fun equals(other: Any?): Boolean {
               log.append("eq($n) "); return other is T && other.n == n
           }
           override fun hashCode(): Int { log.append("hash($n) "); return n }
           override fun toString(): String { log.append("str($n) "); return "T$n" }
       }
       class Seq(val xs: List<T>, val throws: Char, val at: Int) : Iterable<T> {
           override fun iterator(): Iterator<T> {
               log.append("iterator "); if (throws == 'i') throw Boom()
               return object : Iterator<T> {
                   var i = 0
                   override fun hasNext(): Boolean {
                       log.append("hasNext "); if (throws == 'h' && i == at) throw Boom()
                       return i < xs.size
                   }
                   override fun next(): T {
                       log.append("next "); if (throws == 'n' && i == at) throw Boom()
                       return xs[i++]
                   }
               }
           }
       }
       // then, for c in 'i', 'h', 'n': s.map { log.append("f "); it }, s.forEach { .. },
       // s.any { ..; false }, s.all { ..; true }, s.none { ..; false }, s.none(), s.any(),
       // s.count(), s.count { ..; true }, s.filter { ..; true }, s.firstOrNull { ..; false },
       // s.first { ..; false }, s.last { ..; false }, s.fold(0) { a, _ -> ..; a },
       // s.forEachIndexed { _, _ -> .. }, s.toList(), s.reversed(), s.sortedWith { _, _ -> ..; 0 },
       // s.indexOf(T(9)), s.joinToString(), s + T(5), s.sumOf { ..; 1 }, s.withIndex().toList(),
       // mutableListOf<T>().addAll(s), printing whether each threw and the log.

   Every walk threw, with the log `iterator ` for c = 'i'; `iterator hasNext next <calls> hasNext `
   for 'h' and the same followed by `next ` for 'n', where <calls> is `f ` for the walks with a
   lambda, `eq(9) ` for indexOf (the ARGUMENT's `equals`, as `element == item` asks), `str(0) ` for
   joinToString and nothing for the rest. `none()` and `any()` returned after `iterator hasNext `,
   their one `hasNext` coming before the throwing one. */
#include "program_collections.h"

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
    /* The calls it makes into the program for the first element. */
    const char *calls;
} Walk;

static const Walk walks[] = {
    {"map", w_map, "f "},
    {"forEach", w_for_each, "f "},
    {"any", w_any, "f "},
    {"all", w_all, "f "},
    {"none", w_none, "f "},
    {"count", w_count, ""},
    {"count { }", w_count_if, "f "},
    {"filter", w_filter, "f "},
    {"firstOrNull", w_first_or_null, "f "},
    {"first", w_first, "f "},
    {"last", w_last, "f "},
    {"fold", w_fold, "f "},
    {"forEachIndexed", w_for_each_indexed, "f "},
    {"toList", w_to_list, ""},
    {"reversed", w_reversed, ""},
    {"sortedWith", w_sorted_with, ""},
    {"indexOf", w_index_of, "eq(9) "},
    {"joinToString", w_join, "str(0) "},
    {"plus", w_plus, ""},
    {"sumOf", w_sum_of, "f "},
    {"withIndex", w_with_index, ""},
    {"addAll", w_add_all, ""},
};

static char expected[128];

static const char *expect(const char *calls, char throwing) {
    kt_int length = 0;
    const char *parts[] = {"iterator ", throwing == 'i' ? NULL : "hasNext next ",
                           throwing == 'i' ? NULL : calls,
                           throwing == 'i' ? NULL : "hasNext ",
                           throwing == 'n' ? "next " : NULL};
    for (unsigned part = 0; part < sizeof(parts) / sizeof(parts[0]); part++) {
        for (const char *at = parts[part]; at != NULL && *at != 0; at++) {
            expected[length++] = *at;
        }
    }
    expected[length] = 0;
    return expected;
}

void kt_program_entry(void) {
    PROGRAM_BEGIN();
    KRef elements[] = {tag(0, ""), tag(1, "")};
    static const char throwing[] = {'i', 'h', 'n'};
    for (unsigned w = 0; w < sizeof(walks) / sizeof(walks[0]); w++) {
        for (unsigned t = 0; t < 3; t++) {
            for (int placeholder = 0; placeholder <= 1; placeholder++) {
                char calls[2] = {throwing[t], 0};
                KRef seq = seq_throwing(seq_of(&seq_type, elements, 2), calls, 1, placeholder);
                call_log_length = 0;
                walks[w].run(seq);
                if (!threw_last() || !log_is(expect(walks[w].calls, throwing[t]))) {
                    kt_int length = 0;
                    while (walks[w].name[length] != 0) {
                        length++;
                    }
                    kt_sys_write(2, walks[w].name, (size_t)length);
                    KT_SYS_FAIL(": the walk did not stop at the program's throwing call\n");
                }
            }
        }
    }

    /* `none()` and `any()` ask `hasNext` once, before the throwing one. */
    for (unsigned t = 1; t < 3; t++) {
        char calls[2] = {throwing[t], 0};
        KRef seq = seq_throwing(seq_of(&seq_type, elements, 2), calls, 1, 0);
        call_log_length = 0;
        CHECK(!kt_iterable_is_empty(seq) && log_is("iterator hasNext "), "none()\n");
        CHECK(kt_iterable_is_not_empty(seq) && log_is("iterator hasNext "), "any()\n");
        CHECK(kt_pending_exception() == NULL, "none() or any() raised\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
