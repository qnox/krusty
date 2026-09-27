/* A walk's `Int` counter past `Int.MAX_VALUE` raises Kotlin's `ArithmeticException` rather than
   wrapping: `count()` of 2^31 elements raises `Count overflow has happened.` at the last one, and
   `indexOf` in 2^31 + 1 elements raises `Index overflow has happened.` before comparing the
   element at index 2^31, having compared every one before it.
   The counters were signed `Int`s incremented past their maximum, which is undefined in C; the
   harness builds every driver with signed overflow trapping, so a counter left signed ends this
   driver with SIGILL instead of the exception.

   The expected answers are Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       class Many(val n: Long) : Iterable<Any> {
           override fun iterator() = object : Iterator<Any> {
               var left = n
               override fun hasNext() = left > 0
               override fun next(): Any { left--; return Unit }
           }
       }
       fun t(label: String, f: () -> Any?) = println("$label " + try { f().toString() }
           catch (e: Throwable) { "threw ${e::class.qualifiedName}: ${e.message}" })
       fun main() {
           t("count 2^31") { Many(2147483648L).count() }
           t("indexOf 2^31+1") { Many(2147483649L).indexOf("x") }
           t("indexOf 2^31") { Many(2147483648L).indexOf("x") }
       }

   which prints `count 2^31 threw java.lang.ArithmeticException: Count overflow has happened.`,
   `indexOf 2^31+1 threw java.lang.ArithmeticException: Index overflow has happened.` and
   `indexOf 2^31 -1`. The same program answers `2147483647` for `Many(2147483647L).count()`,
   `Count overflow` for `count { true }` of 2^31, and `Index overflow` for `forEachIndexed` over
   2^31 + 1 elements after the action saw index 2147483647. `count { }`, `forEachIndexed` and
   `withIndex()` count through the same two checks as the walks here, and are not repeated because
   each costs 2^31 steps (and `forEachIndexed` a boxed index per step). */
#include "collections_later_tiers.h"

typedef struct Many {
    KObjectHeader header;
    int64_t size;
} Many;

typedef struct ManyIterator {
    KObjectHeader header;
    int64_t left;
} ManyIterator;

static KObjectHeader unit_object;

static kt_boolean many_has_next(KRef self) { return ((const ManyIterator *)self)->left > 0; }

static KRef many_next(KRef self) {
    ((ManyIterator *)self)->left--;
    return (KRef)&unit_object;
}

static const KType many_iterator_type = {
    .name = "ManyIterator",
    .name_length = sizeof("ManyIterator") - 1,
    .instance_size = sizeof(ManyIterator),
    .super = &kt_type_any,
    .walk_has_next = many_has_next,
    .walk_next = many_next,
};

static KRef many_iterator(KRef self) {
    ManyIterator *iterator =
        (ManyIterator *)kt_gc_allocate(&many_iterator_type, sizeof(ManyIterator));
    iterator->left = ((const Many *)self)->size;
    return (KRef)iterator;
}

static const KType many_type = {
    .name = "Many",
    .name_length = sizeof("Many") - 1,
    .instance_size = sizeof(Many),
    .super = &kt_type_any,
    .walk_iterator = many_iterator,
};

static KRef many(int64_t size) {
    Many *made = (Many *)kt_gc_allocate(&many_type, sizeof(Many));
    made->size = size;
    return (KRef)made;
}

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

static kt_boolean raised(const char *message, kt_int length) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    return thrown != NULL && type_of(thrown) == &kt_type_arithmetic_exception &&
           text_is(kt_throwable_message(thrown), message, length);
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    unit_object.type = &kt_type_any;

    (void)kt_iterable_count(many(2147483648LL));
    CHECK(raised("Count overflow has happened.", 28), "count() of 2^31 elements\n");

    (void)kt_iterable_index_of(many(2147483649LL), (KRef)&probe_object);
    CHECK(raised("Index overflow has happened.", 28), "indexOf in 2^31 + 1 elements\n");
    kt_sys_write(1, "OK\n", 3);
}
