/* `lazy` on its ordinary path, in Kotlin's order: a lazy runs its initializer once, on the first
   read, and renders a fixed text until then. `Delegates.observable` and `Delegates.notNull` are
   lowered from the KLIB, not the runtime; the box regressions cover them.

   The expected behaviour is Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       fun main() {
           var runs = 0
           val lz = lazy { runs++; "v$runs" }
           println("${lz.isInitialized()} $lz $runs")
           println("${lz.value} ${lz.value} $runs ${lz.isInitialized()} $lz")
       }

   which prints `false Lazy value not initialized yet. 0` and `v1 v1 1 true v1`. */
#include "driver_checks.h"

static int runs;

/* The lazy's initializer, a `Function0` answering in the slot every function value declares. */
static KRef initializer_invoke(KRef self) {
    (void)self;
    runs++;
    return kt_string_utf8("v1", 2);
}

static const kt_fn initializer_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                           (kt_fn)kt_any_to_string, (kt_fn)initializer_invoke};

static const KType initializer_type = {
    .name = "Initializer",
    .name_length = sizeof("Initializer") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = initializer_vtable,
    .vtable_length = 4,
};

static kt_boolean renders(KRef value, const char *bytes, kt_int length) {
    return text_is(kt_to_string(value), bytes, length);
}

#define RENDERS(value, literal) renders(value, literal, sizeof(literal) - 1)

void kt_program_entry(void) {
    DRIVER_BEGIN();
    /* lazy */
    KRef lazy = kt_lazy_of((KRef)kt_gc_allocate(&initializer_type, sizeof(KObjectHeader)));
    CHECK(!kt_lazy_is_initialized(lazy) && runs == 0, "a lazy computed before its first read\n");
    CHECK(RENDERS(lazy, "Lazy value not initialized yet.") && runs == 0,
          "an unread lazy's toString\n");
    KRef value = kt_lazy_value(lazy);
    CHECK(text_is(value, "v1", 2) && kt_lazy_value(lazy) == value && runs == 1,
          "a lazy ran its initializer other than once\n");
    CHECK(kt_lazy_is_initialized(lazy) && RENDERS(lazy, "v1"), "a read lazy's state\n");

    CHECK(kt_pending_exception() == NULL, "a lazy raised\n");
    kt_sys_write(1, "OK\n", 3);
}
