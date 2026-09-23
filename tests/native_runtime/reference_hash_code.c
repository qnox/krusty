/* A callable reference hashes from exactly what its equality reads — the declaration's identity and
   the bound receiver — as `31 * target + receiver.hashCode()` on Kotlin's wrapping `Int`. The
   arithmetic is on the unsigned ring: a code address truncated to 32 bits times 31 overflows a
   signed `int` for nearly every address, and that overflow is undefined in C, so an optimizer was
   free to answer differently for two references that are equal. Built with
   `-fsanitize=signed-integer-overflow -fsanitize-trap=signed-integer-overflow` this driver traps on
   the signed version; built plainly it pins the value the unsigned version answers. */
#include "krusty_rt.h"
#include "krusty_sys.h"

/* The shape the generator gives a bound reference: the header, then the receiver it bound. */
typedef struct BoundReference {
    KObjectHeader header;
    KRef receiver;
} BoundReference;

static const uint32_t bound_offsets[] = {offsetof(BoundReference, receiver)};
static const kt_fn reference_vtable[] = {(kt_fn)kt_reference_equals, (kt_fn)kt_reference_hash_code,
                                         (kt_fn)kt_any_to_string};

static KType bound_type = {
    .name = "f",
    .name_length = 1,
    .instance_size = sizeof(BoundReference),
    .reference_count = 1,
    .reference_offsets = bound_offsets,
    .super = &kt_type_any,
    .vtable = reference_vtable,
    .vtable_length = 3,
    .reference_receiver_offset = offsetof(BoundReference, receiver),
};

static KRef bound(KRef receiver) {
    BoundReference *reference =
        (BoundReference *)kt_gc_allocate(&bound_type, sizeof(BoundReference));
    reference->receiver = receiver;
    return (KRef)reference;
}

__attribute__((noinline)) static void run(void) {
    /* A target in mapped memory, whose address is high enough that its low 32 bits times 31 leave
       the signed range; the receiver's hash is `Int.MAX_VALUE`, so the addition does too. */
    uint8_t *mapped = (uint8_t *)kt_map(4096);
    const void *target = mapped + 0xFF0;
    bound_type.reference_target = target;

    KRef receiver = kt_box_int(0x7FFFFFFF);
    KRef first = bound(receiver);
    KRef second = bound(kt_box_int(0x7FFFFFFF));
    if (!kt_equals(first, second)) {
        KT_SYS_FAIL("two references to one declaration on equal receivers are not equal\n");
    }
    kt_int expected =
        (kt_int)(31u * (uint32_t)(uintptr_t)target + (uint32_t)kt_hash_code(receiver));
    if (kt_hash_code(first) != expected || kt_hash_code(second) != expected) {
        KT_SYS_FAIL("a bound reference does not hash as 31 * target + receiver.hashCode()\n");
    }
}

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    run();
    kt_sys_write(1, "OK\n", 3);
}
