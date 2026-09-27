/* Kotlin's `Char.isWhitespace()` on the JVM is `Character.isWhitespace || Character.isSpaceChar`,
   and U+0085 (NEXT LINE) is neither: it is a control character outside Java's list of whitespace
   controls. The runtime's table used to include it, so `"\u0085".isBlank()` answered true and
   `trim` removed it. Every code point the JVM does count is checked alongside, so the table is held
   to the JVM's set in both directions. */
#include "standins.h"

/* The UTF-8 encoding of `code`, which is below U+10000. */
static kt_int encode(uint32_t code, char *out) {
    if (code < 0x80u) {
        out[0] = (char)code;
        return 1;
    }
    if (code < 0x800u) {
        out[0] = (char)(0xC0u | (code >> 6));
        out[1] = (char)(0x80u | (code & 0x3Fu));
        return 2;
    }
    out[0] = (char)(0xE0u | (code >> 12));
    out[1] = (char)(0x80u | ((code >> 6) & 0x3Fu));
    out[2] = (char)(0x80u | (code & 0x3Fu));
    return 3;
}

/* Every code unit for which the JVM's `Character.isWhitespace(c) || Character.isSpaceChar(c)`
   holds. */
static const uint32_t jvm_whitespace[] = {
    0x09,   0x0A,   0x0B,   0x0C,   0x0D,   0x1C,   0x1D,   0x1E,   0x1F,   0x20,   0xA0,
    0x1680, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009,
    0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000,
};

void kt_program_entry(void) {
    DRIVER_BEGIN();
    unsigned next = 0;
    for (uint32_t code = 0; code < 0x3001u; code++) {
        kt_boolean expected = next < sizeof(jvm_whitespace) / sizeof(jvm_whitespace[0])
                              && jvm_whitespace[next] == code;
        if (expected) {
            next++;
        }
        static char bytes[3];
        kt_int length = encode(code, bytes);
        if (kt_string_is_blank(kt_string_utf8(bytes, length)) != expected) {
            DRIVER_CHECK(code != 0x85u, "U+0085 is not whitespace on the JVM");
            DRIVER_CHECK(0, "isBlank disagrees with the JVM on a single code unit");
        }
    }

    /* `trim` keeps what `isBlank` does not count. */
    KRef next_line = kt_string_utf8("\xC2\x85x\xC2\x85", 5);
    DRIVER_CHECK(driver_text_is(kt_string_trim(next_line), "\xC2\x85x\xC2\x85", 5),
                 "trim removed U+0085");
    KRef spaced = kt_string_utf8(" \xC2\xA0x\xE3\x80\x80", 7);
    DRIVER_CHECK(driver_text_is(kt_string_trim(spaced), "x", 1), "trim kept a JVM whitespace");

    kt_sys_write(1, "OK\n", 3);
}
