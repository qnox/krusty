/* A lambda that throws ends the walk that called it, and the exception it threw is the one that
   propagates -- Kotlin's `map`, `any`, `sumOf`, `sortedWith` and the rest are ordinary loops over
   the lambda, so a throw leaves them at once. `kt_throw` records the exception and comes back, and
   the lambda then answers NULL; the walks used to take that NULL as the answer: `any` and `sumOf`
   unboxed it and crashed, `sortWith` stopped the program as a comparator that "answered nothing",
   `map` went on calling the transform for every element left, and `first { }` replaced the
   lambda's exception with its own "no element matching".

   Every walk below is handed a list of three and a lambda that throws; each must call the lambda
   ONCE and come back with that lambda's exception pending. */
#include "collections_later_tiers.h"

static int invocations;

static KRef raise_one(KRef self, KRef element) {
    (void)self;
    (void)element;
    invocations++;
    kt_throw(kt_throwable_new(&kt_type_illegal_argument_exception, NULL));
    return NULL;
}

static KRef raise_two(KRef self, KRef first, KRef second) {
    (void)first;
    return raise_one(self, second);
}

FUNCTION_VALUE(throwing, raise_one)
FUNCTION_VALUE(throwing_two, raise_two)

static KRef list;

/* Each check runs one walk from a clean slate. */
static void begin(void) {
    invocations = 0;
    CHECK(kt_pending_exception() == NULL, "an exception was left pending\n");
}

#define EXPECT_STOPPED(literal)                                                                    \
    do {                                                                                           \
        CHECK(invocations == 1, literal " called the lambda again after it threw\n");              \
        CHECK(took(&kt_type_illegal_argument_exception),                                           \
              literal " did not come back with the lambda's exception\n");                         \
    } while (0)

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    list = kt_mutable_list_new();
    kt_gc_add_global_root((void **)&list);
    kt_mutable_list_add(list, kt_box_int(3));
    kt_mutable_list_add(list, kt_box_int(1));
    kt_mutable_list_add(list, kt_box_int(2));

    begin();
    (void)kt_iterable_map(list, throwing);
    EXPECT_STOPPED("map");
    begin();
    kt_iterable_for_each(list, throwing);
    EXPECT_STOPPED("forEach");
    begin();
    kt_iterable_for_each_indexed(list, throwing_two);
    EXPECT_STOPPED("forEachIndexed");
    begin();
    (void)kt_iterable_fold(list, kt_box_int(0), throwing_two);
    EXPECT_STOPPED("fold");
    begin();
    (void)kt_iterable_any(list, throwing);
    EXPECT_STOPPED("any");
    begin();
    (void)kt_iterable_all(list, throwing);
    EXPECT_STOPPED("all");
    begin();
    (void)kt_iterable_none(list, throwing);
    EXPECT_STOPPED("none");
    begin();
    (void)kt_iterable_count_matching(list, throwing);
    EXPECT_STOPPED("count");
    begin();
    (void)kt_iterable_filter(list, throwing);
    EXPECT_STOPPED("filter");
    begin();
    (void)kt_iterable_filter_not(list, throwing);
    EXPECT_STOPPED("filterNot");
    begin();
    (void)kt_iterable_first_or_null(list, throwing);
    EXPECT_STOPPED("firstOrNull");
    begin();
    (void)kt_iterable_first_matching(list, throwing);
    EXPECT_STOPPED("first");
    begin();
    (void)kt_iterable_last_matching(list, throwing);
    EXPECT_STOPPED("last");
    begin();
    (void)kt_iterable_sum_of_int(list, throwing);
    EXPECT_STOPPED("sumOf Int");
    begin();
    (void)kt_iterable_sum_of_long(list, throwing);
    EXPECT_STOPPED("sumOf Long");
    begin();
    (void)kt_iterable_sum_of_double(list, throwing);
    EXPECT_STOPPED("sumOf Double");
    begin();
    (void)kt_iterable_sorted_with(list, throwing_two);
    EXPECT_STOPPED("sortedWith");
    begin();
    kt_list_sort_with(list, throwing_two);
    EXPECT_STOPPED("sortWith");
    CHECK(kt_unbox_int(kt_list_get(list, 0)) == 3 && kt_unbox_int(kt_list_get(list, 1)) == 1
              && kt_unbox_int(kt_list_get(list, 2)) == 2,
          "a sort whose comparator threw moved elements\n");
    kt_sys_write(1, "OK\n", 3);
}
