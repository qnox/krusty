/* `"$a$b"` renders each operand through its own `toString`, and one of the program's may throw. The
   JVM stops there: `b.toString()` never runs when `a.toString()` threw, and no text is built. The
   concatenation used to take the NULL a throwing `toString` hands back for the text `null`, render
   the other operand — running its `toString` with an exception already in flight — and allocate
   the result. */
#include "standins.h"

/* What each operand's `toString` throws, so the exception in flight is named by identity. Both are
   made before any allocation is counted, and kept as collector roots, so that a count taken around
   a concatenation sees only what the concatenation itself allocates. */
static KRef left_exception;
static KRef right_exception;
/* What each operand's `toString` threw last. */
static KRef thrown_by_left;
static KRef thrown_by_right;
static unsigned right_calls;

/* A `toString` that throws, as generated code does: the exception pending and NULL returned. */
static KRef left_to_string(KRef self) {
    (void)self;
    thrown_by_left = left_exception;
    kt_throw(thrown_by_left);
    return NULL;
}

/* One that counts its calls and throws a DIFFERENT exception, so a call made after the left one
   threw would show both in the count and in the pending slot, which its throw would take over. */
static KRef right_to_string(KRef self) {
    (void)self;
    right_calls++;
    thrown_by_right = right_exception;
    kt_throw(thrown_by_right);
    return NULL;
}

static const kt_fn left_vtable[] = {NULL, NULL, (kt_fn)left_to_string};
static const kt_fn right_vtable[] = {NULL, NULL, (kt_fn)right_to_string};

static const KType left_type = {.name = "Left",
                                .name_length = sizeof("Left") - 1,
                                .instance_size = sizeof(KObjectHeader),
                                .super = &kt_type_any,
                                .vtable = left_vtable,
                                .vtable_length = 3};

static const KType right_type = {.name = "Right",
                                 .name_length = sizeof("Right") - 1,
                                 .instance_size = sizeof(KObjectHeader),
                                 .super = &kt_type_any,
                                 .vtable = right_vtable,
                                 .vtable_length = 3};

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KObjectHeader left = {&left_type};
    KObjectHeader right = {&right_type};
    kt_gc_add_global_root((void **)&left_exception);
    kt_gc_add_global_root((void **)&right_exception);
    left_exception = kt_throwable_new(&kt_type_index_out_of_bounds_exception, NULL);
    right_exception = kt_throwable_new(&kt_type_index_out_of_bounds_exception, NULL);

    /* The LEFT operand throws: the right one's `toString` never runs, there is no text, nothing is
       allocated, and the exception in flight is the very one the left threw. */
    kt_gc_collect();
    size_t live = kt_gc_live_objects();
    KRef result = kt_string_plus((KRef)&left, (KRef)&right);
    size_t live_after = kt_gc_live_objects();
    KRef thrown = driver_take_pending();
    DRIVER_CHECK(right_calls == 0, "the right operand's toString ran after the left one threw");
    DRIVER_CHECK(thrown_by_left != NULL && thrown == thrown_by_left,
                 "the exception in flight is not the one the left operand threw");
    DRIVER_CHECK(result == NULL || driver_type_of(result) != &kt_type_string,
                 "a concatenation whose left operand threw built text");
    DRIVER_CHECK(live_after == live, "a concatenation whose left operand threw allocated");

    /* Only the RIGHT operand throws: its `toString` runs once, there is no text, nothing is
       allocated, and its exception is the one in flight. */
    kt_gc_collect();
    KRef text = kt_string_utf8("text", 4);
    live = kt_gc_live_objects();
    result = kt_string_plus(text, (KRef)&right);
    live_after = kt_gc_live_objects();
    thrown = driver_take_pending();
    DRIVER_CHECK(right_calls == 1, "the right operand's toString did not run exactly once");
    DRIVER_CHECK(thrown_by_right != NULL && thrown == thrown_by_right,
                 "the exception in flight is not the one the right operand threw");
    DRIVER_CHECK(result == NULL || driver_type_of(result) != &kt_type_string,
                 "a concatenation whose right operand threw built text");
    DRIVER_CHECK(live_after == live, "a concatenation whose right operand threw allocated");

    /* Operands that render without throwing still concatenate. */
    DRIVER_CHECK(driver_text_is(kt_string_plus(text, kt_string_utf8("!", 1)), "text!", 5),
                 "\"text\" + \"!\"");
    DRIVER_CHECK(driver_take_pending() == NULL, "a concatenation that threw nothing raised");

    kt_sys_write(1, "OK\n", 3);
}
