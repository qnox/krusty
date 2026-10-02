/* A walk's `Int` count past `Int.MAX_VALUE` raises Kotlin's `ArithmeticException` rather than
   wrapping: `count()` of 2^31 elements raises `Count overflow has happened.` at the last one, having
   read every element, and `count()` of 2^31 - 1 answers `Int.MAX_VALUE`. The counters were signed
   `Int`s incremented past their maximum, which is undefined in C; the harness builds every driver
   with signed overflow trapping, so a counter left signed ends this driver with SIGILL instead.

   The driver prints what ordinary walks answer and how many elements each left unread, and the
   harness compares the lines with what `walk_count_overflow.kt` answers under the reference
   kotlinc. The walks that reach the check read 2^31 elements, which the JVM cannot do within the
   harness's limit on a loaded machine, so the driver pins those answers, recorded in the program:
   it starts every walk's counter at 2^31 - 2 (see `walk_counter.h`), so two elements reach the
   count a walk of 2^31 reaches. */
#include "walk_counter.h"

static KRef always(KRef self, KRef element) {
    (void)self;
    (void)element;
    return kt_box_boolean(1);
}

/* Every other element, starting with the first, as the program's `seen++ % 2 == 0` says. */
static kt_int seen;

static KRef every_other(KRef self, KRef element) {
    (void)self;
    (void)element;
    return kt_box_boolean(seen++ % 2 == 0);
}

FUNCTION_VALUE(f_always, always)
FUNCTION_VALUE(f_every_other, every_other)

void kt_program_entry(void) {
    WALK_COUNTER_BEGIN();

    KRef walked = many(3);
    show_walk("count() of 3 elements", kt_iterable_count(walked), walked);
    walked = many(0);
    show_walk("count() of no elements", kt_iterable_count(walked), walked);
    walked = many(5);
    show_walk("count { every other } of 5 elements",
              kt_iterable_count_matching(walked, f_every_other), walked);

    /* The walks that reach the check, pinned (see `walk_count_overflow.kt`). */
    kt_walk_counter_origin = 2147483646u;
    walked = many(1);
    CHECK(kt_iterable_count(walked) == 2147483647 && kt_pending_exception() == NULL &&
              ((const Many *)walked)->left == 0,
          "count() of 2^31 - 1 elements\n");
    walked = many(2);
    (void)kt_iterable_count(walked);
    CHECK(raised_overflow("Count overflow has happened.") && ((const Many *)walked)->left == 0,
          "count() of 2^31 elements\n");
    walked = many(1);
    CHECK(kt_iterable_count_matching(walked, f_always) == 2147483647 &&
              kt_pending_exception() == NULL,
          "count { } of 2^31 - 1 elements\n");
    walked = many(2);
    (void)kt_iterable_count_matching(walked, f_always);
    CHECK(raised_overflow("Count overflow has happened.") && ((const Many *)walked)->left == 0,
          "count { } of 2^31 elements\n");

    kt_walk_counter_origin = 0;
    kt_sys_write(1, "OK\n", 3);
}
