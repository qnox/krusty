/* A runtime entry that calls into the program -- an `equals`, `hashCode` or `toString` it
   overrides -- stops where that call raised, and the exception the program raised is the one left
   in flight. `kt_throw` records into one slot and comes back, so an entry that went on after the
   call would overwrite it: `assertEquals` whose operand's `equals` threw used to go on to build its
   report and raise an `AssertionError` over the program's exception, and a `catch` for the real
   one missed. The kotlin.test assertions, `Throwable(cause)` and a bound reference's `equals` and
   `hashCode` each have a case here. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#define CHECK(condition, what)                                                                     \
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

static void raise_from_program(void) {
    raised_by_program = kt_throwable_new(&kt_type_illegal_state_exception, NULL);
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

__attribute__((noinline)) static void assertions(KRef raising, KRef unprintable) {
    KRef one = kt_box_int(1);

    kt_assert_equals(raising, one, NULL);
    CHECK(program_exception_pending(), "assertEquals whose expected's equals threw raised over it");
    kt_assert_equals(unprintable, one, NULL);
    CHECK(program_exception_pending(),
          "assertEquals whose expected's toString threw raised over it");
    kt_assert_equals(one, unprintable, kt_string_utf8("m", 1));
    CHECK(program_exception_pending(), "assertEquals whose actual's toString threw raised over it");

    kt_assert_same(unprintable, one, NULL);
    CHECK(program_exception_pending(), "assertSame whose expected's toString threw raised over it");
    kt_assert_same(one, unprintable, NULL);
    CHECK(program_exception_pending(), "assertSame whose actual's toString threw raised over it");
    kt_assert_not_same(unprintable, unprintable, NULL);
    CHECK(program_exception_pending(), "assertNotSame whose toString threw raised over it");

    KRef was = kt_throwable_new(&unprintable_failure_type, NULL);
    kt_assert_failed_to_throw(NULL, &kt_type_illegal_argument_exception, was);
    CHECK(program_exception_pending(),
          "assertFailsWith whose caught exception's toString threw raised over it");
}

__attribute__((noinline)) static void throwable_from_cause(KRef unprintable) {
    /* `Throwable(cause)` renders the cause for its message before it constructs anything, so a
       rendering that raised leaves nothing constructed: Kotlin never reaches the constructor. */
    KRef made = kt_throwable_new_from_cause(&kt_type_runtime_exception, unprintable);
    CHECK(program_exception_pending(), "Throwable(cause) whose toString threw lost the exception");
    CHECK(made == NULL, "Throwable(cause) whose toString threw constructed an exception anyway");
}

__attribute__((noinline)) static void bound_references(KRef raising) {
    KRef first = bound(raising);
    KRef second = bound(kt_gc_allocate(&raising_type, sizeof(KObjectHeader)));
    kt_equals(first, second);
    CHECK(program_exception_pending(), "a bound reference whose receiver's equals threw lost it");
    kt_hash_code(first);
    CHECK(program_exception_pending(), "a bound reference whose receiver's hashCode threw lost it");
}

void kt_program_entry(void) {
    /* A collection may run inside any allocation, and it scans the stack from here. */
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
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
