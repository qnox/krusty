/* `a..b` over a program's own `Comparable` is ordered by that class's `compareTo`, which the range
   carries. The range used to ask `kt_compare_any`, which knows only the builtin comparables and
   ends the program for anything else, so a range of a program's type could be built but not asked
   anything. `Version` below is such a type: the runtime has never heard of it.

   Its `compareTo` is the program's code and may raise. A range member stops at the first raise:
   `contains` does not make its second comparison, and the exception pending afterwards is the one
   that comparison threw. */
#include "driver_checks.h"

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
/* What the raising comparison threw last, so the pending exception is checked by identity. */
static KRef thrown;

/* `Version.compareTo`: by number. A version numbered `raising_number` raises instead, and answers a
   positive placeholder that a caller ignoring the raise would read as an order. */
static kt_int version_compare(KRef a, KRef b) {
    comparisons++;
    const Version *left = (const Version *)a;
    const Version *right = (const Version *)b;
    if (left->number == raising_number || right->number == raising_number) {
        thrown = kt_throwable_new(&kt_type_null_pointer_exception, NULL);
        kt_throw(thrown);
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
    DRIVER_BEGIN();
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

    /* The value's own comparison with the start raises: that exception is the one pending, and the
       comparison with the end never runs. What `contains` returns beside the exception is not
       looked at: a caller reads nothing before it has looked for one. */
    raising_number = 7;
    comparisons = 0;
    (void)kt_comparable_range_contains(range, version(7));
    CHECK(thrown != NULL && kt_pending_exception() == thrown,
          "the compareTo exception is not the one pending\n");
    CHECK(comparisons == 1, "contains compared again after compareTo raised\n");
    kt_clear_pending();

    /* A bound's comparison raises in `isEmpty`: the same, and only the one comparison. */
    comparisons = 0;
    thrown = NULL;
    (void)kt_comparable_range_is_empty(kt_comparable_range(version(7), two, version_compare));
    CHECK(thrown != NULL && kt_pending_exception() == thrown,
          "the compareTo exception is not the one pending\n");
    CHECK(comparisons == 1, "isEmpty compared more than once\n");
    kt_clear_pending();

    kt_sys_write(1, "OK\n", 3);
}
