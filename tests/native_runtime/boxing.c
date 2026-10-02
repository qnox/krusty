/* Boxing each primitive kind: the box has the kind's descriptor and unboxes to the same value, and
   the small values the JVM caches come back as the same object every time. Outside the cached
   ranges Kotlin promises no identity; the runtime makes a fresh box, which is also the answer the
   JVM gives for the values checked here.

   The expected identities and renderings are Kotlin's, from this program compiled and run with the
   reference kotlinc 2.4.10 on the JVM:

       fun same(a: Any?, b: Any?) = a === b
       fun <T> box(x: T): Any? = x
       fun main() {
           println("${same(box(-128), box(-128))} ${same(box(127), box(127))} ${same(box(128), box(128))}")
           println("${same(box(-129), box(-129))} ${same(box(-128L), box(-128L))} ${same(box(128L), box(128L))}")
           val s: Short = 127; val s2: Short = 128
           println("${same(box(s), box(s))} ${same(box(s2), box(s2))}")
           val b1: Byte = -128; val b2: Byte = 127
           println("${same(box(b1), box(b1))} ${same(box(b2), box(b2))}")
           println("${same(box('\u0000'), box('\u0000'))} ${same(box('\u007f'), box('\u007f'))}")
           println("${same(box('\u0080'), box('\u0080'))} ${same(box(true), box(true))}")
           println("${same(box(false), box(false))} ${box(Long.MIN_VALUE) == box(0L)} ${box(Long.MIN_VALUE)}")
           println("${box(0.5f)} ${box(0.5)} ${box(0.5) == box(0.0)} ${box(1000)}")
       }

   which prints `true true false`, `false true false`, `true false`, `true true`, `true true`,
   `false true`, `true false -9223372036854775808` and `0.5 0.5 false 1000`. */
#include "driver_checks.h"

static kt_boolean renders(KRef box, const char *bytes, kt_int length) {
    return text_is(kt_to_string(box), bytes, length);
}

#define RENDERS(box, literal) renders(box, literal, sizeof(literal) - 1)

void kt_program_entry(void) {
    DRIVER_BEGIN();

    /* Int: -128..127 cached, 128 and -129 fresh. */
    CHECK(kt_box_int(-128) == kt_box_int(-128), "Int -128 is not cached\n");
    CHECK(kt_box_int(127) == kt_box_int(127), "Int 127 is not cached\n");
    CHECK(kt_box_int(128) != kt_box_int(128), "Int 128 came back as one object\n");
    CHECK(kt_box_int(-129) != kt_box_int(-129), "Int -129 came back as one object\n");
    CHECK(type_of(kt_box_int(5)) == &kt_type_int && kt_unbox_int(kt_box_int(5)) == 5, "Int 5\n");
    CHECK(kt_unbox_int(kt_box_int(1000)) == 1000 && RENDERS(kt_box_int(1000), "1000"),
          "Int 1000\n");

    /* Long: the same range, and the whole value picks the slot. */
    CHECK(kt_box_long(-128) == kt_box_long(-128), "Long -128 is not cached\n");
    CHECK(kt_box_long(127) == kt_box_long(127), "Long 127 is not cached\n");
    CHECK(kt_box_long(128) != kt_box_long(128), "Long 128 came back as one object\n");
    KRef min = kt_box_long((kt_long)0x8000000000000000ull);
    CHECK(min != kt_box_long(0) && type_of(min) == &kt_type_long,
          "Long.MIN_VALUE took zero's box\n");
    CHECK(kt_unbox_long(min) == (kt_long)0x8000000000000000ull &&
              RENDERS(min, "-9223372036854775808"),
          "Long.MIN_VALUE\n");
    CHECK(kt_unbox_long(kt_box_long(0)) == 0, "Long 0\n");

    /* Short: -128..127. */
    CHECK(kt_box_short(127) == kt_box_short(127), "Short 127 is not cached\n");
    CHECK(kt_box_short(-128) == kt_box_short(-128), "Short -128 is not cached\n");
    CHECK(kt_box_short(128) != kt_box_short(128), "Short 128 came back as one object\n");
    CHECK(type_of(kt_box_short(-300)) == &kt_type_short &&
              kt_unbox_short(kt_box_short(-300)) == -300,
          "Short -300\n");

    /* Byte: every value. */
    CHECK(kt_box_byte(-128) == kt_box_byte(-128), "Byte -128 is not cached\n");
    CHECK(kt_box_byte(127) == kt_box_byte(127), "Byte 127 is not cached\n");
    CHECK(type_of(kt_box_byte(-5)) == &kt_type_byte && kt_unbox_byte(kt_box_byte(-5)) == -5,
          "Byte -5\n");

    /* Char: 0..127. */
    CHECK(kt_box_char(0) == kt_box_char(0), "Char 0 is not cached\n");
    CHECK(kt_box_char(0x7F) == kt_box_char(0x7F), "Char 127 is not cached\n");
    CHECK(kt_box_char(0x80) != kt_box_char(0x80), "Char 128 came back as one object\n");
    CHECK(type_of(kt_box_char(0xE9)) == &kt_type_char && kt_unbox_char(kt_box_char(0xE9)) == 0xE9,
          "Char U+00E9\n");
    CHECK(RENDERS(kt_box_char('c'), "c"), "Char 'c' renders\n");

    /* Boolean: both. */
    CHECK(kt_box_boolean(1) == kt_box_boolean(1), "true is not cached\n");
    CHECK(kt_box_boolean(0) == kt_box_boolean(0), "false is not cached\n");
    CHECK(kt_box_boolean(1) != kt_box_boolean(0), "true and false share a box\n");
    CHECK(type_of(kt_box_boolean(1)) == &kt_type_boolean && kt_unbox_boolean(kt_box_boolean(1)) &&
              !kt_unbox_boolean(kt_box_boolean(0)),
          "Boolean values\n");

    /* Float and Double: never cached, and a fraction survives (0.5 is not 0.0's box). */
    KRef half_float = kt_box_float(0.5f);
    CHECK(type_of(half_float) == &kt_type_float && kt_unbox_float(half_float) == 0.5f &&
              RENDERS(half_float, "0.5"),
          "Float 0.5\n");
    KRef half = kt_box_double(0.5);
    CHECK(type_of(half) == &kt_type_double && kt_unbox_double(half) == 0.5 && RENDERS(half, "0.5"),
          "Double 0.5\n");
    CHECK(kt_unbox_double(kt_box_double(0.0)) == 0.0 && kt_box_double(0.5) != kt_box_double(0.5),
          "Double boxes\n");

    CHECK(kt_pending_exception() == NULL, "boxing raised\n");
    kt_sys_write(1, "OK\n", 3);
}
