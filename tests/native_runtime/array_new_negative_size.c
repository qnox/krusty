/* A negative array size is an exception a program can catch, not the end of the program: every
   array kind raises `IllegalArgumentException` with the size as its message and makes no array. It
   used to stop the program with `krusty: negative array size`.

   The TYPE is Kotlin/Native's, as `StringBuilder(-1)`'s is (see `builder_negative_capacity`): the
   Kotlin/Native 2.4.10 stdlib's array constructors (`IntArray(n)`, `Array(n) { }` and kin)
   allocate through `AllocArrayInstance`, which calls `ThrowIllegalArgumentException` for a negative
   size (read from the disassembly of the linux_x64 `libstdlib-cache.a`); `kt_array_new` is that
   allocation. The MESSAGE is the JVM's, the size; Kotlin/Native's has none. Kotlin/JVM throws
   Java's `NegativeArraySizeException`, a type Kotlin does not declare.

   The driver prints what each allocation raised, and the harness compares the lines with what
   `array_new_negative_size.kt` answers under the reference kotlinc, the type on each line a
   declared divergence. */
#include "transcript.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    static const struct {
        const char *label;
        const KType *kind;
        kt_int size;
    } cases[] = {
        {"Array<Any?>(-1)", &kt_type_array, -1},
        {"ByteArray(-1)", &kt_type_byte_array, -1},
        {"IntArray(-1)", &kt_type_int_array, -1},
        {"LongArray(-1)", &kt_type_long_array, -1},
        {"CharArray(-1)", &kt_type_char_array, -1},
        {"DoubleArray(-1)", &kt_type_double_array, -1},
        {"ULongArray(-1)", &kt_type_ulong_array, -1},
        {"ByteArray(Int.MIN_VALUE)", &kt_type_byte_array, (kt_int)0x80000000u},
    };
    for (unsigned at = 0; at < sizeof(cases) / sizeof(cases[0]); at++) {
        KRef array = kt_array_new(cases[at].kind, cases[at].size);
        CHECK(array == NULL, "a negative size made an array\n");
        say(cases[at].label);
        say(" ");
        KRef thrown = kt_pending_exception();
        CHECK(thrown != NULL, "a negative size raised nothing\n");
        kt_clear_pending();
        say_thrown(thrown);
        say("\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
