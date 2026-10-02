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

/* The observable and the log its callback writes, as `Holder` above keeps them. Both are collector
   roots: the callback reaches them from static storage. */
static KRef observable;
static KRef log_builder;
static KRef obs_property;
static KRef obs_name;
static int callbacks;

/* `{ prop, old, new -> log.append("[${prop.name}:$old->$new read=$obs]") }`, as a `Function3`. The
   property is the very object the write handed over, and it is named here the way the generated
   code names it. */
static KRef callback_invoke(KRef self, KRef property, KRef old, KRef new_value) {
    (void)self;
    callbacks++;
    CHECK(property == obs_property, "the callback was not handed the property\n");
    (void)kt_string_builder_append(log_builder, kt_string_utf8("[", 1));
    (void)kt_string_builder_append(log_builder, obs_name);
    (void)kt_string_builder_append(log_builder, kt_string_utf8(":", 1));
    (void)kt_string_builder_append(log_builder, old);
    (void)kt_string_builder_append(log_builder, kt_string_utf8("->", 2));
    (void)kt_string_builder_append(log_builder, new_value);
    (void)kt_string_builder_append(log_builder, kt_string_utf8(" read=", 6));
    (void)kt_string_builder_append(log_builder, kt_rw_property_get(observable, obs_name));
    (void)kt_string_builder_append(log_builder, kt_string_utf8("]", 1));
    return NULL;
}

static const kt_fn callback_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                        (kt_fn)kt_any_to_string, (kt_fn)callback_invoke};

static const KType callback_type = {
    .name = "Callback",
    .name_length = sizeof("Callback") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = callback_vtable,
    .vtable_length = 4,
};

/* The `KProperty` object the delegation passes; the runtime only hands it on. */
static const KType property_type = {
    .name = "Property",
    .name_length = sizeof("Property") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
};

static kt_boolean renders(KRef value, const char *bytes, kt_int length) {
    return text_is(kt_to_string(value), bytes, length);
}

#define RENDERS(value, literal) renders(value, literal, sizeof(literal) - 1)

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_gc_add_global_root((void **)&observable);
    kt_gc_add_global_root((void **)&log_builder);
    kt_gc_add_global_root((void **)&obs_property);
    kt_gc_add_global_root((void **)&obs_name);

    /* lazy */
    KRef lazy = kt_lazy_of((KRef)kt_gc_allocate(&initializer_type, sizeof(KObjectHeader)));
    CHECK(!kt_lazy_is_initialized(lazy) && runs == 0, "a lazy computed before its first read\n");
    CHECK(RENDERS(lazy, "Lazy value not initialized yet.") && runs == 0,
          "an unread lazy's toString\n");
    KRef value = kt_lazy_value(lazy);
    CHECK(text_is(value, "v1", 2) && kt_lazy_value(lazy) == value && runs == 1,
          "a lazy ran its initializer other than once\n");
    CHECK(kt_lazy_is_initialized(lazy) && RENDERS(lazy, "v1"), "a read lazy's state\n");

    /* observable */
    log_builder = kt_string_builder_new();
    obs_name = kt_string_utf8("obs", 3);
    obs_property = (KRef)kt_gc_allocate(&property_type, sizeof(KObjectHeader));
    observable = kt_observable(kt_string_utf8("init", 4),
                               (KRef)kt_gc_allocate(&callback_type, sizeof(KObjectHeader)));
    (void)kt_string_builder_append(log_builder, kt_string_utf8("read=", 5));
    (void)kt_string_builder_append(log_builder, kt_rw_property_get(observable, obs_name));
    (void)kt_string_builder_append(log_builder, kt_string_utf8(";", 1));
    kt_rw_property_set(observable, obs_property, kt_string_utf8("one", 3));
    (void)kt_string_builder_append(log_builder, kt_string_utf8("read=", 5));
    (void)kt_string_builder_append(log_builder, kt_rw_property_get(observable, obs_name));
    (void)kt_string_builder_append(log_builder, kt_string_utf8(";", 1));
    kt_rw_property_set(observable, obs_property, kt_string_utf8("two", 3));
    static const char expected_log[] =
        "read=init;[obs:init->one read=one]read=one;[obs:one->two read=two]";
    CHECK(callbacks == 2 && text_is(log_builder, expected_log, sizeof(expected_log) - 1),
          "the observable's log\n");

    /* notNull */
    KRef not_null = kt_not_null_var();
    KRef nn = kt_string_utf8("nn", 2);
    (void)kt_rw_property_get(not_null, nn);
    KRef thrown = kt_pending_exception();
    CHECK(thrown != NULL && type_of(thrown) == &kt_type_illegal_state_exception,
          "a notNull read before a write did not throw IllegalStateException\n");
    static const char message[] = "Property nn should be initialized before get.";
    CHECK(text_is(kt_throwable_message(thrown), message, sizeof(message) - 1),
          "the notNull message\n");
    kt_clear_pending();
    KRef set = kt_string_utf8("set", 3);
    kt_rw_property_set(not_null, NULL, set);
    CHECK(kt_rw_property_get(not_null, nn) == set, "a notNull read after a write\n");
    KRef again = kt_string_utf8("again", 5);
    kt_rw_property_set(not_null, NULL, again);
    CHECK(kt_rw_property_get(not_null, nn) == again, "a notNull read after a second write\n");

    CHECK(kt_pending_exception() == NULL, "a delegate raised\n");
    kt_sys_write(1, "OK\n", 3);
}
