/* `a..b` over a program's own `Comparable` is ordered by that class's `compareTo`, which the range
   carries. The range used to ask `kt_compare_any`, which knows only the builtin comparables and
   ends the program for anything else, so a range of a program's type could be built but not asked
   anything. `Version` below is such a type: the runtime has never heard of it.

   Its `compareTo` is the program's code and may raise. A range member stops at the first raise:
   `contains` does not make its second comparison, and neither member answers `true` for a
   comparison that raised. */
#include "later_tiers.h"

typedef struct Version {
    KObjectHeader header;
    kt_int number;
} Version;

static const KType version_type = {
    .name = "Version",
    .name_length = sizeof("Version") - 1,
    .instance_size = sizeof(Version),
    .super = &kt_type_any,
};

static int comparisons;
static kt_int raising_number = -1;

/* `Version.compareTo`: by number. A version numbered `raising_number` raises instead, and answers a
   positive placeholder that a caller ignoring the raise would read as an order. */
static kt_int version_compare(KRef a, KRef b) {
    comparisons++;
    const Version *left = (const Version *)a;
    const Version *right = (const Version *)b;
    if (left->number == raising_number || right->number == raising_number) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
        return 1;
    }
    return left->number < right->number ? -1 : left->number > right->number ? 1 : 0;
}

static KRef version(kt_int number) {
    Version *made = (Version *)kt_gc_allocate(&version_type, sizeof(Version));
    made->number = number;
    return (KRef)made;
}

void kt_program_entry(void) {
    KRef two = version(2);
    KRef five = version(5);
    KRef range = kt_comparable_range(two, five, version_compare);
    CHECK(type_of(range) == &kt_type_comparable_range, "not a comparable range\n");
    CHECK(kt_comparable_range_start(range) == two, "start is not the first bound\n");
    CHECK(kt_comparable_range_end(range) == five, "endInclusive is not the second bound\n");

    CHECK(!kt_comparable_range_is_empty(range), "2..5 is empty\n");
    CHECK(kt_comparable_range_is_empty(kt_comparable_range(five, two, version_compare)),
          "5..2 is not empty\n");
    CHECK(!kt_comparable_range_is_empty(kt_comparable_range(two, two, version_compare)),
          "2..2 is empty\n");
    CHECK(kt_comparable_range_contains(range, version(2)), "2..5 does not contain 2\n");
    CHECK(kt_comparable_range_contains(range, version(3)), "2..5 does not contain 3\n");
    CHECK(kt_comparable_range_contains(range, version(5)), "2..5 does not contain 5\n");
    CHECK(!kt_comparable_range_contains(range, version(1)), "2..5 contains 1\n");
    CHECK(!kt_comparable_range_contains(range, version(6)), "2..5 contains 6\n");
    CHECK(kt_pending_exception() == NULL, "an ordinary comparison raised\n");

    /* The value's own comparison with the start raises: nothing is contained, and the comparison
       with the end never runs. */
    raising_number = 7;
    comparisons = 0;
    CHECK(!kt_comparable_range_contains(range, version(7)), "a raising comparison was contained\n");
    CHECK(kt_pending_exception() != NULL, "the compareTo exception did not propagate\n");
    CHECK(comparisons == 1, "contains compared again after compareTo raised\n");
    kt_clear_pending();

    /* A bound's comparison raises: the range is not empty on the strength of the placeholder. */
    comparisons = 0;
    CHECK(!kt_comparable_range_is_empty(kt_comparable_range(version(7), two, version_compare)),
          "a raising comparison made a range empty\n");
    CHECK(kt_pending_exception() != NULL, "the compareTo exception did not propagate\n");
    CHECK(comparisons == 1, "isEmpty compared more than once\n");
    kt_clear_pending();

    kt_sys_write(1, "OK\n", 3);
}
