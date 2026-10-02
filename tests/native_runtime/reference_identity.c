/* Callable-reference equality is Kotlin's, not identity: two references are equal when they name
   the same declaration and bound equal receivers, or both bound none -- whichever site made them. A
   reference to another declaration, a bound one against an unbound one, a bound one on a receiver
   that is not equal, and anything that is not a reference, is not equal. The receiver's `equals` is
   asked only once the declarations matched, and only by a bound reference (an unbound one compares
   its absent receiver without calling anything).

   The driver prints each comparison's answer and the calls it made into `R`, and the harness
   compares the lines with what `reference_identity.kt` answers under the reference kotlinc. The
   references here are what the generator emits, one descriptor per SITE naming its declaration by
   address, so `R::f` written twice is two descriptors naming one declaration.

   Hashing is where the platforms differ: the JVM's `FunctionReference.hashCode` reads the owner,
   name and signature and never the receiver, while Kotlin/Native's `KFunctionImpl.hashCode` calls
   the receiver's `hashCode` (its linux_x64 stdlib cache calls `Any.hashCode` on `receiver`). This
   runtime follows Kotlin/Native, a declared divergence. */
#include "driver_checks.h"
#include "transcript.h"

/* `R(n)`, whose `equals` and `hashCode` log the calls made into it, as the Kotlin program's
   does. */
typedef struct R {
    KObjectHeader header;
    kt_int n;
} R;

static char calls[256];
static size_t calls_length;

static void log_call(const char *what, kt_int n) {
    const char digit[2] = {(char)('0' + n), 0};
    for (const char *parts[] = {what, "(", digit, ") ", NULL}, **part = parts; *part != NULL;
         part++) {
        for (const char *c = *part; *c != 0 && calls_length < sizeof(calls); c++) {
            calls[calls_length++] = *c;
        }
    }
}

static kt_boolean r_equals(KRef self, KRef other);
static kt_int r_hash_code(KRef self) {
    log_call("hash", ((const R *)self)->n);
    return ((const R *)self)->n;
}

static const kt_fn r_vtable[] = {(kt_fn)r_equals, (kt_fn)r_hash_code, (kt_fn)kt_any_to_string};

static const KType r_type = {
    .name = "R",
    .name_length = 1,
    .instance_size = sizeof(R),
    .super = &kt_type_any,
    .vtable = r_vtable,
    .vtable_length = 3,
};

static kt_boolean r_equals(KRef self, KRef other) {
    log_call("eq", ((const R *)self)->n);
    return other != NULL && type_of(other) == &r_type &&
           ((const R *)other)->n == ((const R *)self)->n;
}

static KRef r(kt_int n) {
    R *made = (R *)kt_gc_allocate(&r_type, sizeof(R));
    made->n = n;
    return (KRef)made;
}

/* `label = answer [calls]`, the calls made since the last line, trimmed, and the log emptied. */
static void show(const char *label, kt_boolean answer) {
    say(label);
    say(answer ? " = true [" : " = false [");
    say_bytes(calls, calls_length > 0 ? calls_length - 1 : 0);
    say("]\n");
    calls_length = 0;
}

/* The shape the generator gives a reference: the header, then the receiver when it bound one. */
typedef struct Reference {
    KObjectHeader header;
    KRef receiver;
} Reference;

static const uint32_t bound_offsets[] = {offsetof(Reference, receiver)};
static const kt_fn reference_vtable[] = {(kt_fn)kt_reference_equals, (kt_fn)kt_reference_hash_code,
                                         (kt_fn)kt_any_to_string};

/* The declarations a reference can name; only their addresses matter. */
static const char declaration_f, declaration_g, declaration_top, declaration_other;

/* One descriptor per SITE, as the generator emits them: `R::f` written twice is two descriptors
   naming one declaration. */
#define UNBOUND(identifier, target)                                                                \
    static const KType identifier = {.name = #identifier,                                          \
                                     .name_length = sizeof(#identifier) - 1,                       \
                                     .instance_size = sizeof(KObjectHeader),                       \
                                     .super = &kt_type_any,                                        \
                                     .vtable = reference_vtable,                                   \
                                     .vtable_length = 3,                                           \
                                     .reference_target = &target};
#define BOUND(identifier, target)                                                                  \
    static const KType identifier = {.name = #identifier,                                          \
                                     .name_length = sizeof(#identifier) - 1,                       \
                                     .instance_size = sizeof(Reference),                           \
                                     .reference_count = 1,                                         \
                                     .reference_offsets = bound_offsets,                           \
                                     .super = &kt_type_any,                                        \
                                     .vtable = reference_vtable,                                   \
                                     .vtable_length = 3,                                           \
                                     .reference_receiver_offset = offsetof(Reference, receiver),   \
                                     .reference_target = &target};

UNBOUND(unbound_f_site1, declaration_f)
UNBOUND(unbound_f_site2, declaration_f)
UNBOUND(unbound_g, declaration_g)
UNBOUND(top_site1, declaration_top)
UNBOUND(top_site2, declaration_top)
UNBOUND(other_ref, declaration_other)
BOUND(bound_f_site1, declaration_f)
BOUND(bound_f_site2, declaration_f)
BOUND(bound_g, declaration_g)

#undef UNBOUND
#undef BOUND

/* A lambda: an object with a vtable of its own and no declaration it names. */
static const KType lambda_type = {
    .name = "lambda",
    .name_length = sizeof("lambda") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .vtable = reference_vtable,
    .vtable_length = 3,
};

static KRef unbound(const KType *type) { return kt_gc_allocate(type, sizeof(KObjectHeader)); }

static KRef bound(const KType *type, KRef receiver) {
    Reference *reference = (Reference *)kt_gc_allocate(type, sizeof(Reference));
    reference->receiver = receiver;
    return (KRef)reference;
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    KRef a = r(1);
    KRef a2 = r(1);
    KRef b = r(2);

    KRef u1 = unbound(&unbound_f_site1);
    KRef u2 = unbound(&unbound_f_site2);
    show("unbound, two sites", kt_equals(u1, u2));
    show("unbound, two sites, identical", u1 == u2);
    show("unbound, two declarations", kt_equals(unbound(&unbound_f_site1), unbound(&unbound_g)));
    show("top-level, two sites", kt_equals(unbound(&top_site1), unbound(&top_site2)));
    show("top-level, two declarations", kt_equals(unbound(&top_site1), unbound(&other_ref)));
    show("bound, one receiver", kt_equals(bound(&bound_f_site1, a), bound(&bound_f_site2, a)));
    show("bound, equal receivers", kt_equals(bound(&bound_f_site1, a), bound(&bound_f_site1, a2)));
    show("bound, receivers that differ",
         kt_equals(bound(&bound_f_site1, a), bound(&bound_f_site1, b)));
    show("bound vs unbound", kt_equals(bound(&bound_f_site1, a), unbound(&unbound_f_site2)));
    show("unbound vs bound", kt_equals(unbound(&unbound_f_site1), bound(&bound_f_site2, a)));
    show("bound, two declarations", kt_equals(bound(&bound_f_site1, a), bound(&bound_g, a)));
    show("reference vs lambda", kt_equals(unbound(&top_site1), unbound(&lambda_type)));
    show("equal unbound references hash alike", kt_hash_code(u1) == kt_hash_code(u2));

    /* Kotlin/Native's hash, which asks the receiver's, where the JVM's does not: a declared
       divergence. */
    show("equal bound references hash alike",
         kt_hash_code(bound(&bound_f_site1, a)) == kt_hash_code(bound(&bound_f_site2, a2)));

    CHECK(kt_pending_exception() == NULL, "a reference raised\n");
    kt_sys_write(1, "OK\n", 3);
}
