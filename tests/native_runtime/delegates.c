/* `lazy`, `Delegates.observable` and `Delegates.notNull` on their ordinary paths, in Kotlin's
   order: a lazy runs its initializer once, on the first read, and renders a fixed text until
   then; an observable stores the new value BEFORE its callback runs, so the callback reads the new
   value, and hands the callback the property and both values; a notNull read before any write
   throws `IllegalStateException("Property <name> should be initialized before get.")`, and after
   a write answers the latest value.

   The expected behaviour is Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       import kotlin.properties.Delegates
       class Holder {
           val log = StringBuilder()
           var obs: String by Delegates.observable("init") { prop, old, new ->
               log.append("[${prop.name}:$old->$new read=$obs]")
           }
           var nn: String by Delegates.notNull()
       }
       fun main() {
           var runs = 0
           val lz = lazy { runs++; "v$runs" }
           println("${lz.isInitialized()} $lz $runs")
           println("${lz.value} ${lz.value} $runs ${lz.isInitialized()} $lz")
           val h = Holder()
           h.log.append("read=${h.obs};")
           h.obs = "one"
           h.log.append("read=${h.obs};")
           h.obs = "two"
           println(h.log)
           try { println(h.nn) } catch (t: IllegalStateException) {
               println("${t::class.simpleName}: ${t.message}")
           }
           h.nn = "set"; println(h.nn); h.nn = "again"; println(h.nn)
       }

   which prints `false Lazy value not initialized yet. 0`, `v1 v1 1 true v1`,
   `read=init;[obs:init->one read=one]read=one;[obs:one->two read=two]`,
   `IllegalStateException: Property nn should be initialized before get.`, `set` and `again`. */
#include "driver_exceptions.h"
#include "native_driver_fixture.h"

static int runs;
static KRef not_null;
static KRef nn_property;
static KRef set;
static KRef again;

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

typedef struct Custom {
    KObjectHeader header;
    KRef held;
} Custom;

static const KType custom_type = {.name = "pkg.CustomDelegate",
                                  .name_length = sizeof("pkg.CustomDelegate") - 1,
                                  .instance_size = sizeof(Custom),
                                  .super = &kt_type_any};
static KRef custom_root;
static KRef held_root;
static KRef new_root;


static void check_unknown_get(void) {
    custom_root = (KRef)kt_gc_allocate(&custom_type, sizeof(Custom));
    held_root = kt_string_utf8("held", 4);
    new_root = kt_string_utf8("new", 3);
    ((Custom *)custom_root)->held = held_root;
    CHECK(native_fixture_read(custom_root, NULL) == NULL,
          "an invalid read returned a value instead of the exception placeholder\n");
    CHECK(took_message(&kt_type_class_cast_exception,
          "class pkg.CustomDelegate cannot be cast to class kotlin.properties.ReadWriteProperty"),
          "the invalid receiver did not fail the checked interface boundary\n");
    CHECK(((Custom *)custom_root)->held == held_root,
          "an invalid delegate's held sentinel was changed\n");
    CHECK(text_is(new_root, "new", 3), "the new-value sentinel was changed\n");
    CHECK(text_is(native_fixture_calls(), "OK", 2), "an invalid receiver invoked an operator\n");
    CHECK(kt_pending_exception() == NULL, "the boundary left an exception in flight\n");
}

static void check_unknown_set(void) {
    custom_root = (KRef)kt_gc_allocate(&custom_type, sizeof(Custom));
    held_root = kt_string_utf8("held", 4);
    new_root = kt_string_utf8("new", 3);
    ((Custom *)custom_root)->held = held_root;
    native_fixture_write(custom_root, NULL, new_root);
    CHECK(took_message(&kt_type_class_cast_exception,
          "class pkg.CustomDelegate cannot be cast to class kotlin.properties.ReadWriteProperty"),
          "the invalid receiver did not fail the checked interface boundary\n");
    CHECK(((Custom *)custom_root)->held == held_root,
          "an invalid delegate's held sentinel was changed\n");
    CHECK(text_is(new_root, "new", 3), "the new-value sentinel was changed\n");
    CHECK(text_is(native_fixture_calls(), "OK", 2), "an invalid receiver invoked an operator\n");
    CHECK(kt_pending_exception() == NULL, "the boundary left an exception in flight\n");
}

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

    kt_gc_add_global_root((void **)&custom_root);
    kt_gc_add_global_root((void **)&held_root);
    kt_gc_add_global_root((void **)&new_root);
    check_unknown_get();
    check_unknown_set();

    /* The exact observable/notNull/property/callback observations above now execute real
       KLIB-produced bodies. The complete portable Kotlin contract also checks callback throws
       and a legitimate user-defined ReadWriteProperty; no C delegate stands in for those bodies. */
    KRef contract = native_fixture_contract();
    CHECK(kt_pending_exception() == NULL && contract != NULL && text_is(contract, "OK", 2),
          "the compiler-generated delegate contract\n");

    /* Preserve the raw positive boundary too: the setter's unused KProperty operand is NULL,
       while reads receive the actual compiler-produced ::nn reference. Both values retain identity. */
    kt_gc_add_global_root((void **)&not_null);
    kt_gc_add_global_root((void **)&nn_property);
    kt_gc_add_global_root((void **)&set);
    kt_gc_add_global_root((void **)&again);
    not_null = native_fixture_not_null();
    nn_property = native_fixture_nn_property();
    CHECK(not_null != NULL && nn_property != NULL && kt_pending_exception() == NULL,
          "the compiled notNull factory and property reference\n");
    (void)native_fixture_read(not_null, nn_property);
    CHECK(took_message(&kt_type_illegal_state_exception,
                      "Property nn should be initialized before get."), "the notNull message\n");
    set = kt_string_utf8("set", 3);
    native_fixture_write(not_null, NULL, set);
    CHECK(native_fixture_read(not_null, nn_property) == set, "a notNull read after a write\n");
    again = kt_string_utf8("again", 5);
    native_fixture_write(not_null, NULL, again);
    CHECK(native_fixture_read(not_null, nn_property) == again,
          "a notNull read after a second write\n");

    CHECK(kt_pending_exception() == NULL, "a delegate raised\n");
    kt_sys_write(1, "OK\n", 3);
}
