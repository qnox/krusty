/* Kotlin's `countLeadingZeroBits` on `Int` and `Long`, computed by the runtime, against exact
   answers: the number of zero bits above the highest set one, which is the whole width for zero. */
#include "krusty_rt.h"
#include "krusty_sys.h"

typedef struct { uint64_t value; kt_int count; } Case;

static const Case ints[] = {
    {0x00000000u, 32}, {0x00000001u, 31}, {0x00000002u, 30}, {0x00000003u, 30},
    {0x000000FFu, 24}, {0x00000100u, 23}, {0x0000FFFFu, 16}, {0x00010000u, 15},
    {0x00FFFFFFu, 8},  {0x01000000u, 7},  {0x7FFFFFFFu, 1},  {0x80000000u, 0},
    {0xFFFFFFFFu, 0},
};

static const Case longs[] = {
    {0x0000000000000000ull, 64}, {0x0000000000000001ull, 63}, {0x00000000FFFFFFFFull, 32},
    {0x0000000100000000ull, 31}, {0x0000FFFFFFFFFFFFull, 16}, {0x0001000000000000ull, 15},
    {0x00FFFFFFFFFFFFFFull, 8},  {0x0100000000000000ull, 7},  {0x7FFFFFFFFFFFFFFFull, 1},
    {0x8000000000000000ull, 0},  {0xFFFFFFFFFFFFFFFFull, 0},
};

static void write_hex(uint64_t bits) {
    char hex[16];
    for (int i = 0; i < 16; i++) hex[i] = "0123456789ABCDEF"[(bits >> (4 * (15 - i))) & 15];
    kt_sys_write(2, hex, 16);
}

static void check(const char *width, uint64_t value, kt_int got, kt_int want) {
    if (got == want) return;
    kt_sys_write(2, width, 4);
    write_hex(value);
    kt_sys_write(2, ": got ", 6);
    char digits[2] = {(char)('0' + got / 10), (char)('0' + got % 10)};
    kt_sys_write(2, digits, 2);
    KT_SYS_FAIL("\n");
}

void kt_program_entry(void) {
    for (size_t i = 0; i < sizeof ints / sizeof ints[0]; i++) {
        check("int  0x", ints[i].value,
              kt_count_leading_zero_bits_int((kt_int)(uint32_t)ints[i].value), ints[i].count);
    }
    for (size_t i = 0; i < sizeof longs / sizeof longs[0]; i++) {
        check("long 0x", longs[i].value,
              kt_count_leading_zero_bits_long((kt_long)longs[i].value), longs[i].count);
    }
    kt_sys_write(1, "OK\n", 3);
}
