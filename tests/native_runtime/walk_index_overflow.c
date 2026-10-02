/* A walk's `Int` index past `Int.MAX_VALUE` raises Kotlin's `ArithmeticException` rather than
   wrapping: `indexOf` in 2^31 + 1 elements raises `Index overflow has happened.` having read the
   element at index 2^31 and before comparing it, and `indexOf` in 2^31 elements answers -1.

   The driver prints what ordinary walks answer and how many elements each left unread, and the
   harness compares the lines with what `walk_index_overflow.kt` answers under the reference
   kotlinc. The walks that reach the check read 2^31 elements, which the JVM cannot do within the
   harness's limit on a loaded machine, so the driver pins those answers, recorded in the program:
   it starts every walk's counter at 2^31 - 1 (see `walk_counter.h`), so the second element has
   index 2^31. */
#include "walk_counter.h"

/* What `indexOf` looks for: an object whose `equals` is identity, so no element matches. */
static const kt_fn probe_vtable[] = {(kt_fn)kt_any_equals, NULL, NULL};
static const KType probe_type = {
    .name = "Probe",
    .name_length = sizeof("Probe") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = probe_vtable,
    .vtable_length = 3,
};
static KObjectHeader probe_object = {&probe_type};

static kt_int actions;
static kt_int last_index;

static KRef act(KRef self, KRef index, KRef element) {
    (void)self;
    (void)element;
    actions++;
    last_index = kt_unbox_int(index);
    return NULL;
}

FUNCTION_VALUE(f_act, act)

/* `indices` gathers the index each action saw, as `"$i "`. */
static char indices[64];
static kt_int indices_length;

static KRef gather(KRef self, KRef index, KRef element) {
    (void)self;
    (void)element;
    actions++;
    last_index = kt_unbox_int(index);
    CHECK(indices_length < 60 && last_index >= 0 && last_index < 10, "an index out of range\n");
    indices[indices_length++] = (char)('0' + last_index);
    indices[indices_length++] = ' ';
    return NULL;
}

FUNCTION_VALUE(f_gather, gather)

void kt_program_entry(void) {
    WALK_COUNTER_BEGIN();

    KRef walked = many(3);
    show_walk("indexOf in 3 elements", kt_iterable_index_of(walked, (KRef)&probe_object), walked);
    walked = many(3);
    kt_iterable_for_each_indexed(walked, f_gather);
    CHECK(kt_pending_exception() == NULL, "forEachIndexed raised\n");
    say("forEachIndexed over 3 elements: ");
    say_bytes(indices, indices_length);
    say_long(((const Many *)walked)->left);
    say(" left\n");
    walked = many(3);
    KRef first = kt_iterator_next(kt_iterable_iterator(kt_iterable_with_index(walked)));
    CHECK(kt_pending_exception() == NULL, "withIndex() raised\n");
    say("withIndex() first of 3 elements: ");
    say_long(kt_indexed_value_index(first));
    say(", ");
    say_long(((const Many *)walked)->left);
    say(" left\n");

    /* The walks that reach the check, pinned (see `walk_index_overflow.kt`). */
    kt_walk_counter_origin = 2147483647u;
    walked = many(1);
    CHECK(kt_iterable_index_of(walked, (KRef)&probe_object) == -1 &&
              kt_pending_exception() == NULL && ((const Many *)walked)->left == 0,
          "indexOf in 2^31 elements\n");
    walked = many(2);
    (void)kt_iterable_index_of(walked, (KRef)&probe_object);
    CHECK(raised_overflow("Index overflow has happened.") && ((const Many *)walked)->left == 0,
          "indexOf in 2^31 + 1 elements\n");

    /* `forEachIndexed`: the action sees index 2^31 - 1, and the next element is read and raises
       before the action runs again. */
    actions = 0;
    walked = many(2);
    kt_iterable_for_each_indexed(walked, f_act);
    CHECK(raised_overflow("Index overflow has happened.") && actions == 1 &&
              last_index == 2147483647 && ((const Many *)walked)->left == 0,
          "forEachIndexed over 2^31 + 1 elements\n");

    /* `withIndex()`: the element at index 2^31 - 1 comes out, and the next `next()` raises before
       it fetches the element. */
    walked = many(2);
    KRef indexing = kt_iterable_iterator(kt_iterable_with_index(walked));
    first = kt_iterator_next(indexing);
    CHECK(kt_pending_exception() == NULL && first != NULL, "withIndex()'s element 2^31 - 1\n");
    (void)kt_iterator_next(indexing);
    CHECK(raised_overflow("Index overflow has happened.") && ((const Many *)walked)->left == 1,
          "withIndex()'s element 2^31\n");

    kt_walk_counter_origin = 0;
    kt_sys_write(1, "OK\n", 3);
}
