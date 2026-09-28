/* One compact behavioral transcript shared with the executable Kotlin oracle in
   `native_runtime_e2e.rs`. Each byte records an observed answer, rather than restating that answer
   as a C expectation: the Rust harness compares these bytes directly with what kotlinc runs. */
#include "krusty_rt.h"
#include "krusty_sys.h"

static kt_boolean equals(KRef self, KRef other) {
    const KType *type = ((const KObjectHeader *)self)->type;
    return ((kt_boolean(*)(KRef, KRef))type->vtable[KT_SLOT_EQUALS])(self, other);
}

static void record(char *out, size_t *length, kt_boolean answer) {
    out[(*length)++] = answer ? '1' : '0';
}

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);

    KRef stepped = kt_range_step(kt_int_range(1, 10), 2);
    KRef reversed = kt_range_reversed(kt_range_step(kt_int_range(1, 9), 3));
    KRef progression = kt_range_step(kt_int_range(1, 3), 1);
    KRef range = kt_int_range(1, 3);
    char out[8];
    size_t length = 0;

    record(out, &length, kt_range_first(stepped) == 1);
    record(out, &length, kt_range_last(stepped) == 9);
    record(out, &length, kt_range_first(reversed) == 7);
    record(out, &length, kt_range_last(reversed) == 1);
    record(out, &length, equals(progression, range));
    record(out, &length, !equals(range, progression));
    record(out, &length,
           kt_range_contains(kt_range_step(kt_ulong_range((kt_long)0x7ffffffffffffffeull,
                                                          (kt_long)0x8000000000000004ull),
                                                3),
                             (kt_long)0x8000000000000001ull));

    kt_sys_write(1, out, length);
    kt_sys_write(1, "OK\n", 3);
}
