/* Kotlin's floating-point `%` and `mod`, computed by the runtime, against exact answers.

   `%` is IEEE's remainder truncated toward zero, which is C's `fmod`: the expected bits below are
   the host C library's `fmod`, which is exact, and `mod` is Kotlin's own definition applied to it,
   `val r = a % b; if (r != 0.0 && r.sign != b.sign) r + b else r`. Cases cover an inexact decimal
   divisor, a dividend far larger than the divisor, subnormals, and every sign combination.

   NaN BITS are not something Kotlin specifies, so what is checked for a NaN is what IEEE 754
   requires of any operation: the result is a QUIET NaN, and one that came from an operand keeps
   that operand's payload and sign. A signaling NaN handed back unchanged is not something an
   arithmetic instruction on any target produces. The invalid operations — a remainder of an
   infinity, or by a zero — answer the runtime's one fixed NaN, `0xFFF8…`, which is x86's default
   NaN and so the one the reference JVM yields for them on that host. `mod` of a NaN is only checked
   to BE a NaN: Kotlin's definition adds `b` to it, and which NaN a hardware sum of a NaN answers is
   the target's choice (RISC-V always answers its canonical one). */
#include "krusty_rt.h"
#include "krusty_sys.h"

/* Each row is the bits of `a`, `b`, `a % b` and `a.mod(b)`; its comment names `a` and `b`. */
typedef struct { uint64_t a, b, rem, mod; } DoubleCase;
typedef struct { uint32_t a, b, rem, mod; } FloatCase;

static const DoubleCase doubles[] = {
    /* 0.1, 0.03 */
    {0x3FB999999999999Aull, 0x3F9EB851EB851EB8ull, 0x3F847AE147AE1480ull, 0x3F847AE147AE1480ull},
    /* -0.1, 0.03 */
    {0xBFB999999999999Aull, 0x3F9EB851EB851EB8ull, 0xBF847AE147AE1480ull, 0x3F947AE147AE1478ull},
    /* 0.1, -0.03 */
    {0x3FB999999999999Aull, 0xBF9EB851EB851EB8ull, 0x3F847AE147AE1480ull, 0xBF947AE147AE1478ull},
    /* -7.5, 2 */
    {0xC01E000000000000ull, 0x4000000000000000ull, 0xBFF8000000000000ull, 0x3FE0000000000000ull},
    /* 1e300, 3 */
    {0x7E37E43C8800759Cull, 0x4008000000000000ull, 0x0000000000000000ull, 0x0000000000000000ull},
    /* -1e300, 7 */
    {0xFE37E43C8800759Cull, 0x401C000000000000ull, 0xBFF0000000000000ull, 0x4018000000000000ull},
    /* MAX, 1e-300 */
    {0x7FEFFFFFFFFFFFFFull, 0x01A56E1FC2F8F359ull, 0x0151C210546B28C0ull, 0x0151C210546B28C0ull},
    /* 123456789, 0.001 */
    {0x419D6F3454000000ull, 0x3F50624DD2F1A9FCull, 0x3F50624B1084C21Cull, 0x3F50624B1084C21Cull},
    /* subnormals */
    {0x0000000000000007ull, 0x0000000000000002ull, 0x0000000000000001ull, 0x0000000000000001ull},
    /* -4, 2: -0.0 */
    {0xC010000000000000ull, 0x4000000000000000ull, 0x8000000000000000ull, 0x8000000000000000ull},
    /* 1, Inf */
    {0x3FF0000000000000ull, 0x7FF0000000000000ull, 0x3FF0000000000000ull, 0x3FF0000000000000ull},
    /* -1, Inf */
    {0xBFF0000000000000ull, 0x7FF0000000000000ull, 0xBFF0000000000000ull, 0x7FF0000000000000ull},
    /* -0.0, 1 */
    {0x8000000000000000ull, 0x3FF0000000000000ull, 0x8000000000000000ull, 0x8000000000000000ull},
    /* Invalid operations answer the fixed NaN: 1, 0 and then Inf, 1. */
    {0x3FF0000000000000ull, 0x0000000000000000ull, 0xFFF8000000000000ull, 0xFFF8000000000000ull},
    {0x7FF0000000000000ull, 0x3FF0000000000000ull, 0xFFF8000000000000ull, 0xFFF8000000000000ull},
    /* A NaN operand comes back quiet, with its payload and sign. */
    {0xFFF8000000000000ull, 0x3FF0000000000000ull, 0xFFF8000000000000ull, 0xFFF8000000000000ull},
    {0x7FF0000000000001ull, 0x3FF0000000000000ull, 0x7FF8000000000001ull, 0x7FF8000000000001ull},
    {0x3FF0000000000000ull, 0xFFF4000000000000ull, 0xFFFC000000000000ull, 0xFFFC000000000000ull},
};

static const FloatCase floats[] = {
    {0x3DCCCCCDu, 0x3CF5C28Fu, 0x3C23D70Eu, 0x3C23D70Eu}, /* 0.1f, 0.03f */
    {0xBDCCCCCDu, 0x3CF5C28Fu, 0xBC23D70Eu, 0x3CA3D708u}, /* -0.1f, 0.03f */
    {0x3DCCCCCDu, 0xBCF5C28Fu, 0x3C23D70Eu, 0xBCA3D708u}, /* 0.1f, -0.03f */
    {0xC0F00000u, 0x40000000u, 0xBFC00000u, 0x3F000000u}, /* -7.5f, 2f */
    {0x7F7FFFFFu, 0x40400000u, 0x00000000u, 0x00000000u}, /* MAX, 3f */
    {0x00000007u, 0x00000002u, 0x00000001u, 0x00000001u}, /* subnormals */
    {0x3F800000u, 0x00000000u, 0xFFC00000u, 0xFFC00000u}, /* 1f, 0f */
    {0xFF800000u, 0x3F800000u, 0xFFC00000u, 0xFFC00000u}, /* -Inf, 1f */
    {0x7F800001u, 0x3F800000u, 0x7FC00001u, 0x7FC00001u}, /* signaling NaN, 1f */
    {0x3F800000u, 0xFFA00000u, 0xFFE00000u, 0xFFE00000u}, /* 1f, signaling NaN */
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

/* Whether `bits`, a value `nibbles` hex digits wide, is a NaN: an all-ones exponent over a
   non-zero significand. */
static int is_nan(uint64_t bits, int nibbles) {
    uint64_t magnitude = nibbles == 16 ? bits & 0x7FFFFFFFFFFFFFFFull : bits & 0x7FFFFFFFull;
    return magnitude > (nibbles == 16 ? 0x7FF0000000000000ull : 0x7F800000ull);
}

/* Fail with the operands, the operation, what came back and what was expected. `any_nan` accepts
   every NaN where a NaN is expected. */
static void check(const char *operation, uint64_t a, uint64_t b, uint64_t got, uint64_t want,
                  int nibbles, int any_nan) {
    if (got == want || (any_nan && is_nan(want, nibbles) && is_nan(got, nibbles))) return;
    kt_sys_write(2, "0x", 2);
    write_hex(a, nibbles);
    kt_sys_write(2, operation, 5);
    write_hex(b, nibbles);
    kt_sys_write(2, ": got 0x", 8);
    write_hex(got, nibbles);
    kt_sys_write(2, ", want 0x", 9);
    write_hex(want, nibbles);
    KT_SYS_FAIL("\n");
}

void kt_program_entry(void) {
    for (size_t i = 0; i < sizeof doubles / sizeof doubles[0]; i++) {
        const DoubleCase *c = &doubles[i];
        kt_double a = double_of(c->a), b = double_of(c->b);
        check(" % 0x", c->a, c->b, bits_of_double(kt_rem_double(a, b)), c->rem, 16, 0);
        check(" m 0x", c->a, c->b, bits_of_double(kt_mod_double(a, b)), c->mod, 16, 1);
    }
    for (size_t i = 0; i < sizeof floats / sizeof floats[0]; i++) {
        const FloatCase *c = &floats[i];
        kt_float a = float_of(c->a), b = float_of(c->b);
        check(" % 0x", c->a, c->b, bits_of_float(kt_rem_float(a, b)), c->rem, 8, 0);
        check(" m 0x", c->a, c->b, bits_of_float(kt_mod_float(a, b)), c->mod, 8, 1);
    }
    kt_sys_write(1, "OK\n", 3);
}
