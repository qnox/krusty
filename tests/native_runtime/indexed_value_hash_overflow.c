/* `IndexedValue(index, value).hashCode()` is `index * 31 + value.hashCode()` on Kotlin's `Int`,
   which wraps. It used to be computed in C's signed `int`, where the overflow is undefined rather
   than a wrap; built with `-fsanitize=signed-integer-overflow -fsanitize-trap` that multiplication
   traps, which is how the defect shows on a host whose optimizer happens to wrap. */
#include "collections_later_tiers.h"

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    kt_int index = INT32_MAX / 16;
    KRef indexed = kt_indexed_value(index, NULL);
    kt_int hash = ((kt_int(*)(KRef))type_of(indexed)->vtable[KT_SLOT_HASH_CODE])(indexed);
    CHECK(hash == (kt_int)((uint32_t)index * 31u), "IndexedValue's hashCode did not wrap\n");
    kt_sys_write(1, "OK\n", 3);
}
