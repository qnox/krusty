/* A runtime entry that calls into the program -- an `equals`, `hashCode` or `toString` it
   overrides -- stops where that call raised, and the exception the program raised is the one left
   in flight. `kt_throw` records into one slot and comes back, so an entry that went on after the
   call would overwrite it: `assertEquals` whose operand's `equals` threw used to go on to build its
   report and raise an `AssertionError` over the program's exception, and a `catch` for the real
   one missed. The kotlin.test assertions, `Throwable(cause)` and a bound reference's `equals` and
   `hashCode` each have a case here; `print` and `println` have
   `print_of_raising_to_string_writes_nothing`.

   `assertEquals` compares `actual == expected`, the ACTUAL operand's `equals`, as kotlin-test's
   `DefaultAsserter` does: an expected operand whose `equals` raises is never asked. The runtime
   asked the expected operand's, and the first case pinned that.

   The driver prints each case's outcome, checking each exception by identity against the one the
   program raised, and the harness compares the lines with what
   `user_code_raise_keeps_first_exception.kt` answers under the reference kotlinc, with kotlin-test
   on its class path. A bound reference's `hashCode` is where the platforms differ: the JVM's
   `FunctionReference.hashCode` never asks the receiver, and Kotlin/Native's `KFunctionImpl.hashCode`
   does, which this runtime follows (see `reference_identity`), so here the receiver's raise
   propagates -- a declared divergence. */
#include "transcript.h"

#define REQUIRE(condition, what)                                                                     \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL(what "\n");                                                                \
        }                                                                                          \
    } while (0)

/* The exception a program member raised last. The driver compares the pending slot against it by
   IDENTITY: another exception of the same class raised over it is exactly the failure to catch. It
   is a root, so it stays the object it names across the allocations between the raise and the
   check. */
static KRef raised_by_program;

PROGRAM_EXCEPTION(boom_type, "Boom")

static void raise_from_program(void) {
    raised_by_program = kt_throwable_new(&boom_type, kt_string_utf8("boom", 4));
    kt_throw(raised_by_program);
}

/* Members that raise and come back with nothing, as generated code does. */
static kt_boolean raising_equals(KRef self, KRef other) {
    (void)self;
    (void)other;
    raise_from_program();
    return false;
}

static kt_int raising_hash_code(KRef self) {
    (void)self;
    raise_from_program();
    return 0;
}

static KRef raising_to_string(KRef self) {
    (void)self;
    raise_from_program();
    return NULL;
}

/* A class of the program whose `equals` alone raises, rendering as `kotlin.Any` does. */
static const kt_fn equals_raising_vtable[] = {(kt_fn)raising_equals, (kt_fn)kt_any_hash_code,
                                              (kt_fn)kt_any_to_string};

static const KType equals_raising_type = {
    .name = "Raising",
    .name_length = sizeof("Raising") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = equals_raising_vtable,
    .vtable_length = 3,
};

/* A class of the program overriding all three of `kotlin.Any`'s members with ones that raise. */
static const kt_fn raising_vtable[] = {(kt_fn)raising_equals, (kt_fn)raising_hash_code,
                                       (kt_fn)raising_to_string};

static const KType raising_type = {
    .name = "Raising",
    .name_length = sizeof("Raising") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = raising_vtable,
    .vtable_length = 3,
};

/* One whose `equals` and `hashCode` are `kotlin.Any`'s and whose `toString` raises, so an entry
   gets past comparing it and reaches rendering it. */
static const kt_fn unprintable_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                           (kt_fn)raising_to_string};

static const KType unprintable_type = {
    .name = "Unprintable",
    .name_length = sizeof("Unprintable") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = unprintable_vtable,
    .vtable_length = 3,
};

/* A subclass of `Throwable` whose `toString` raises: what `assertFailsWith` reports as `was`. */
typedef struct UnprintableFailure {
    KObjectHeader header;
    KRef message;
    KRef cause;
} UnprintableFailure;

static const uint32_t failure_offsets[] = {offsetof(UnprintableFailure, message),
                                           offsetof(UnprintableFailure, cause)};

static const KType unprintable_failure_type = {
    .name = "UnprintableFailure",
    .name_length = sizeof("UnprintableFailure") - 1,
    .instance_size = sizeof(UnprintableFailure),
    .reference_count = 2,
    .reference_offsets = failure_offsets,
    .super = &kt_type_runtime_exception,
    .vtable = unprintable_vtable,
    .vtable_length = 3,
};

/* The shape the generator gives a bound reference: the header, then the receiver it bound. */
typedef struct BoundReference {
    KObjectHeader header;
    KRef receiver;
} BoundReference;

static const uint32_t bound_offsets[] = {offsetof(BoundReference, receiver)};
static const kt_fn reference_vtable[] = {(kt_fn)kt_reference_equals, (kt_fn)kt_reference_hash_code,
                                         (kt_fn)kt_any_to_string};
static const char reference_declaration;

static const KType bound_type = {
    .name = "f",
    .name_length = 1,
    .instance_size = sizeof(BoundReference),
    .reference_count = 1,
    .reference_offsets = bound_offsets,
    .super = &kt_type_any,
    .vtable = reference_vtable,
    .vtable_length = 3,
    .reference_receiver_offset = offsetof(BoundReference, receiver),
    .reference_target = &reference_declaration,
};

static KRef bound(KRef receiver) {
    BoundReference *reference =
        (BoundReference *)kt_gc_allocate(&bound_type, sizeof(BoundReference));
    reference->receiver = receiver;
    return (KRef)reference;
}

/* Whether what is in flight is the program's own exception, and nothing else. Clears the slot, as
   the `catch` that takes it would, so each case starts with nothing pending. */
static kt_boolean program_exception_pending(void) {
    KRef thrown = kt_pending_exception();
    kt_clear_pending();
    kt_boolean kept = thrown != NULL && thrown == raised_by_program;
    raised_by_program = NULL;
    return kept;
}

/* The transcript's line for a case whose outcome is the program's own exception: `label = threw
   Boom`, once the exception in flight is checked to be the very one the program raised. */
static void threw_program_exception(const char *label) {
    REQUIRE(program_exception_pending(), "the program's exception was not the one in flight");
    say(label);
    say(" = threw ");
    say_bytes_of(boom_type.simple_name, boom_type.simple_name_length);
    say("\n");
}

__attribute__((noinline)) static void assertions(KRef raising, KRef unprintable) {
    KRef one = kt_box_int(1);

    KRef equals_raising = kt_gc_allocate(&equals_raising_type, sizeof(KObjectHeader));
    kt_assert_equals(equals_raising, one, NULL);
    KRef failure = kt_pending_exception();
    REQUIRE(raised_by_program == NULL && failure != NULL &&
              kt_is_instance(failure, &kt_type_assertion_error),
          "assertEquals asked the expected operand's equals");
    kt_clear_pending();
    say("assertEquals(Raising(), 1) = threw ");
    say_bytes_of(driver_type_of(failure)->simple_name, driver_type_of(failure)->simple_name_length);
    say("\n");
    kt_assert_equals(one, raising, NULL);
    threw_program_exception("assertEquals(1, Raising())");
    kt_assert_equals(unprintable, one, NULL);
    threw_program_exception("assertEquals(u, 1)");
    kt_assert_equals(one, unprintable, kt_string_utf8("m", 1));
    threw_program_exception("assertEquals(1, u, m)");

    kt_assert_same(unprintable, one, NULL);
    threw_program_exception("assertSame(u, 1)");
    kt_assert_same(one, unprintable, NULL);
    threw_program_exception("assertSame(1, u)");
    kt_assert_not_same(unprintable, unprintable, NULL);
    threw_program_exception("assertNotSame(u, u)");

    KRef was = kt_throwable_new(&unprintable_failure_type, NULL);
    kt_assert_failed_to_throw(NULL, &kt_type_illegal_argument_exception, was);
    threw_program_exception("assertFailsWith, UnprintableException thrown");
}

__attribute__((noinline)) static void throwable_from_cause(KRef unprintable) {
    /* `Throwable(cause)` renders the cause for its message before it constructs anything, so a
       rendering that raised leaves nothing constructed: Kotlin never reaches the constructor. */
    KRef made = kt_throwable_new_from_cause(&kt_type_runtime_exception, unprintable);
    threw_program_exception("RuntimeException(UnprintableException())");
    REQUIRE(made == NULL, "Throwable(cause) whose toString threw constructed an exception anyway");
}

__attribute__((noinline)) static void bound_references(KRef raising) {
    KRef first = bound(raising);
    KRef second = bound(kt_gc_allocate(&raising_type, sizeof(KObjectHeader)));
    kt_equals(first, second);
    threw_program_exception("u::f == Unprintable()::f");
    kt_hash_code(first);
    threw_program_exception("(u::f).hashCode()");
}

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    DRIVER_BEGIN();
    kt_gc_add_global_root((void **)&raised_by_program);

    KRef raising = kt_gc_allocate(&raising_type, sizeof(KObjectHeader));
    KRef unprintable = kt_gc_allocate(&unprintable_type, sizeof(KObjectHeader));
    assertions(raising, unprintable);
    throwable_from_cause(unprintable);
    bound_references(raising);

    /* Every case left the slot empty, so the check the generated entry makes before exiting
       returns. */
    kt_check_uncaught();
    kt_sys_write(1, "OK\n", 3);
}
