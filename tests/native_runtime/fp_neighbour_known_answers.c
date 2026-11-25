/* Kotlin's `nextUp` and `nextDown` on `Double` and `Float`, computed by the runtime, against exact
   answers.

   Each is the adjacent representable value toward positive or negative infinity: the bits step by
   one away from or toward zero, either zero steps to the smallest subnormal of the direction's
   sign, an infinity stays where it is in the direction it already points, and a NaN comes back
   unchanged. Every row is the bits of the operand, of `nextUp` and of `nextDown`. */
#include "krusty_rt.h"
#include "krusty_sys.h"

typedef struct { uint64_t value, up, down; } DoubleCase;
typedef struct { uint32_t value, up, down; } FloatCase;

static const DoubleCase doubles[] = {
    /* 1.0 */
    {0x3FF0000000000000ull, 0x3FF0000000000001ull, 0x3FEFFFFFFFFFFFFFull},
    /* -1.0 */
    {0xBFF0000000000000ull, 0xBFEFFFFFFFFFFFFFull, 0xBFF0000000000001ull},
    /* 0.0 and -0.0 both step to a subnormal of the direction's own sign */
    {0x0000000000000000ull, 0x0000000000000001ull, 0x8000000000000001ull},
    {0x8000000000000000ull, 0x0000000000000001ull, 0x8000000000000001ull},
    /* the smallest subnormal and its negation cross to a zero */
    {0x0000000000000001ull, 0x0000000000000002ull, 0x0000000000000000ull},
    {0x8000000000000001ull, 0x8000000000000000ull, 0x8000000000000002ull},
    /* the largest subnormal steps into the smallest normal and back */
    {0x000FFFFFFFFFFFFFull, 0x0010000000000000ull, 0x000FFFFFFFFFFFFEull},
    {0x0010000000000000ull, 0x0010000000000001ull, 0x000FFFFFFFFFFFFFull},
    /* the largest finite value reaches the infinity upward */
    {0x7FEFFFFFFFFFFFFFull, 0x7FF0000000000000ull, 0x7FEFFFFFFFFFFFFEull},
    {0xFFEFFFFFFFFFFFFFull, 0xFFEFFFFFFFFFFFFEull, 0xFFF0000000000000ull},
    /* an infinity stays put in its own direction and steps to the extreme finite value back */
    {0x7FF0000000000000ull, 0x7FF0000000000000ull, 0x7FEFFFFFFFFFFFFFull},
    {0xFFF0000000000000ull, 0xFFEFFFFFFFFFFFFFull, 0xFFF0000000000000ull},
    /* NaNs come back with their bits */
    {0x7FF8000000000000ull, 0x7FF8000000000000ull, 0x7FF8000000000000ull},
    {0xFFF8000000000001ull, 0xFFF8000000000001ull, 0xFFF8000000000001ull},
};

static const FloatCase floats[] = {
    {0x3F800000u, 0x3F800001u, 0x3F7FFFFFu}, /* 1.0f */
    {0xBF800000u, 0xBF7FFFFFu, 0xBF800001u}, /* -1.0f */
    {0x00000000u, 0x00000001u, 0x80000001u}, /* 0.0f */
    {0x80000000u, 0x00000001u, 0x80000001u}, /* -0.0f */
    {0x00000001u, 0x00000002u, 0x00000000u}, /* smallest subnormal */
    {0x80000001u, 0x80000000u, 0x80000002u},
    {0x007FFFFFu, 0x00800000u, 0x007FFFFEu}, /* largest subnormal */
    {0x7F7FFFFFu, 0x7F800000u, 0x7F7FFFFEu}, /* largest finite */
    {0xFF7FFFFFu, 0xFF7FFFFEu, 0xFF800000u},
    {0x7F800000u, 0x7F800000u, 0x7F7FFFFFu}, /* Inf */
    {0xFF800000u, 0xFF7FFFFFu, 0xFF800000u}, /* -Inf */
    {0x7FC00000u, 0x7FC00000u, 0x7FC00000u}, /* NaN */
    {0xFFC00001u, 0xFFC00001u, 0xFFC00001u},
};

static kt_double double_of(uint64_t bits) {
    union { uint64_t bits; kt_double value; } pun = {bits};
    return pun.value;
}

static uint64_t bits_of_double(kt_double value) {
    union { kt_double value; uint64_t bits; } pun = {value};
    return pun.bits;
}

static kt_float float_of(uint32_t bits) {
    union { uint32_t bits; kt_float value; } pun = {bits};
    return pun.value;
}

static uint32_t bits_of_float(kt_float value) {
    union { kt_float value; uint32_t bits; } pun = {value};
    return pun.bits;
}

static void write_hex(uint64_t bits, int nibbles) {
    char hex[16];
    for (int i = 0; i < nibbles; i++) {
        hex[i] = "0123456789ABCDEF"[(bits >> (4 * (nibbles - 1 - i))) & 15];
    }
    kt_sys_write(2, hex, (size_t)nibbles);
}

static void check(const char *operation, uint64_t value, uint64_t got, uint64_t want, int nibbles) {
    if (got == want) return;
    size_t length = 0;
    while (operation[length]) length++;
    kt_sys_write(2, operation, length);
    write_hex(value, nibbles);
    kt_sys_write(2, ": got 0x", 8);
    write_hex(got, nibbles);
    kt_sys_write(2, ", want 0x", 9);
    write_hex(want, nibbles);
    KT_SYS_FAIL("\n");
}

void kt_program_entry(void) {
    for (size_t i = 0; i < sizeof doubles / sizeof doubles[0]; i++) {
        const DoubleCase *c = &doubles[i];
        kt_double value = double_of(c->value);
        check("nextUp 0x", c->value, bits_of_double(kt_next_up_double(value)), c->up, 16);
        check("nextDown 0x", c->value, bits_of_double(kt_next_down_double(value)), c->down, 16);
    }
    for (size_t i = 0; i < sizeof floats / sizeof floats[0]; i++) {
        const FloatCase *c = &floats[i];
        kt_float value = float_of(c->value);
        check("nextUp 0x", c->value, bits_of_float(kt_next_up_float(value)), c->up, 8);
        check("nextDown 0x", c->value, bits_of_float(kt_next_down_float(value)), c->down, 8);
    }
    kt_sys_write(1, "OK\n", 3);
}
