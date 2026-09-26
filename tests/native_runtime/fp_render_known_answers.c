/* `Double.toString` and `Float.toString`, rendered by the runtime, against the strings the JVM
   prints for the same values. Each case is a bit pattern rather than a literal so that what is
   rendered is exactly the value named, and each expected string is the JVM's (JDK 19 and later,
   which is what the reference compiler runs on): the shortest decimal that reads back as the value,
   in Java's layout — plain between 10^-3 and 10^7, `d.dddEn` outside it, and at least one digit
   after the point either way.

   The cases are the ones that decide something: the two layout boundaries on either side, values
   whose shortest decimal is found only after a rounded-up digit carries off the front (`1.0E-7`,
   `1.0E23`), the smallest subnormals where Java's two-digit rule overrules the shortest one-digit
   decimal (`4.9E-324`, not `5E-324`), the extremes of each format, and the values that are not
   numbers. */
#include "krusty_rt.h"
#include "krusty_sys.h"

typedef struct { uint64_t bits; const char *want; } DoubleCase;
typedef struct { uint32_t bits; const char *want; } FloatCase;

static const DoubleCase doubles[] = {
    {0x3FB999999999999Aull, "0.1"},
    {0x3FD3333333333334ull, "0.30000000000000004"}, /* 0.1 + 0.2 */
    {0x3FF0000000000000ull, "1.0"},
    {0xBFF8000000000000ull, "-1.5"},
    {0x4059000000000000ull, "100.0"},
    {0x3F50624DD2F1A9FCull, "0.001"},
    {0x3F1A36E2EB1C432Dull, "1.0E-4"},
    {0x3EE4F8B588E368F1ull, "1.0E-5"},
    /* 1e-7 and 1e23 lie just below their power of ten: their shortest spelling is found by
       rounding nines up, which carries off the front and moves the exponent. */
    {0x3E7AD7F29ABCAF48ull, "1.0E-7"},
    {0x3F60624DD2F1A9FCull, "0.002"},
    {0x416312CFE0000000ull, "9999999.0"},
    {0x416312D000000000ull, "1.0E7"},
    {0x4202A05F20000000ull, "1.0E10"},
    {0x4415AF1D78B58C40ull, "1.0E20"},
    {0x447C7E83209E90B2ull, "8.41E21"},
    {0x44B52D02C7E14AF6ull, "1.0E23"},
    {0x01A56E1FC2F8F359ull, "1.0E-300"},
    {0x01AA74FE1C1E8908ull, "1.2345678901234568E-300"},
    {0x0010000000000000ull, "2.2250738585072014E-308"},
    {0x7FEFFFFFFFFFFFFFull, "1.7976931348623157E308"},
    {0x0000000000000001ull, "4.9E-324"},
    {0x0000000000000002ull, "9.9E-324"},
    {0x0000000000000003ull, "1.5E-323"},
    {0x0000000000000058ull, "4.35E-322"},
    {0x8000000000000001ull, "-4.9E-324"},
    {0x0000000000000000ull, "0.0"},
    {0x8000000000000000ull, "-0.0"},
    {0x7FF0000000000000ull, "Infinity"},
    {0xFFF0000000000000ull, "-Infinity"},
    {0x7FF8000000000000ull, "NaN"},
    {0xFFF8000000000001ull, "NaN"},
};

static const FloatCase floats[] = {
    {0x3DCCCCCDu, "0.1"},
    {0x3EAAAAABu, "0.33333334"},
    {0x3F800000u, "1.0"},
    {0x42C80000u, "100.0"},
    {0x4B800000u, "1.6777216E7"},
    {0x50000026u, "8.589974E9"},
    {0x501502F9u, "1.0E10"},
    {0x00000001u, "1.4E-45"},
    {0x00000002u, "2.8E-45"},
    {0x00800000u, "1.1754944E-38"},
    {0x7F7FFFFFu, "3.4028235E38"},
    {0x80000000u, "-0.0"},
    {0xFF800000u, "-Infinity"},
    {0x7FC00000u, "NaN"},
};

static kt_double double_of(uint64_t bits) {
    union { uint64_t bits; kt_double value; } pun = {bits};
    return pun.value;
}

static kt_float float_of(uint32_t bits) {
    union { uint32_t bits; kt_float value; } pun = {bits};
    return pun.value;
}

static size_t length_of(const char *text) {
    size_t length = 0;
    while (text[length] != '\0') length++;
    return length;
}

static void write_hex(uint64_t bits, int nibbles) {
    char hex[16];
    for (int i = 0; i < nibbles; i++) {
        hex[i] = "0123456789ABCDEF"[(bits >> (4 * (nibbles - 1 - i))) & 15];
    }
    kt_sys_write(2, hex, (size_t)nibbles);
}

/* Fail with the case's bits, what was rendered and what the JVM renders. */
static void check(uint64_t bits, int nibbles, const char *got, kt_int length, const char *want) {
    size_t expected = length_of(want);
    int same = length >= 0 && (size_t)length == expected;
    for (size_t i = 0; same && i < expected; i++) same = got[i] == want[i];
    if (same) return;
    kt_sys_write(2, "rendering 0x", 12);
    write_hex(bits, nibbles);
    kt_sys_write(2, ": got \"", 7);
    if (length > 0 && length <= 32) kt_sys_write(2, got, (size_t)length);
    kt_sys_write(2, "\", want \"", 9);
    kt_sys_write(2, want, expected);
    KT_SYS_FAIL("\"\n");
}

void kt_program_entry(void) {
    for (size_t i = 0; i < sizeof doubles / sizeof doubles[0]; i++) {
        char out[32];
        kt_int length = kt_render_double(double_of(doubles[i].bits), out);
        check(doubles[i].bits, 16, out, length, doubles[i].want);
    }
    for (size_t i = 0; i < sizeof floats / sizeof floats[0]; i++) {
        char out[32];
        kt_int length = kt_render_float(float_of(floats[i].bits), out);
        check(floats[i].bits, 8, out, length, floats[i].want);
    }
    kt_sys_write(1, "OK\n", 3);
}
