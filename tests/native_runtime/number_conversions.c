/* `kotlin.Number`'s six conversions on a value reached as an object: a floating-point source
   saturates at the target's ends and answers zero for NaN, a narrow target (`toShort`, `toByte`)
   narrows what `toInt` answers, and a wider integer truncates.

   Every expected value is Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       fun main() {
           val ns: List<Number> = listOf(Double.NaN, 1e10, -1e10, 3.9, -3.9, 1e19, -1e19,
               0x1_0000_0001L, 40000, 200, Long.MAX_VALUE, Float.NaN, 3.0e9f, 1.5f,
               (-5).toByte(), 300.toShort())
           for (n in ns) println("$n ${n.toByte()} ${n.toShort()} ${n.toInt()} ${n.toLong()} " +
               "${n.toFloat()} ${n.toDouble()}")
       }

   whose rows are the table below; a NaN float or double answer is checked as NaN. */
#include "driver_checks.h"

typedef struct Row {
    KRef number;
    kt_byte to_byte;
    kt_short to_short;
    kt_int to_int;
    kt_long to_long;
    kt_float to_float;
    kt_double to_double;
} Row;

static kt_boolean same_float(kt_float a, kt_float b) { return (a != a && b != b) || a == b; }

static kt_boolean same_double(kt_double a, kt_double b) { return (a != a && b != b) || a == b; }

static void check(const Row *row) {
    CHECK(kt_number_to_byte(row->number) == row->to_byte, "toByte\n");
    CHECK(kt_number_to_short(row->number) == row->to_short, "toShort\n");
    CHECK(kt_number_to_int(row->number) == row->to_int, "toInt\n");
    CHECK(kt_number_to_long(row->number) == row->to_long, "toLong\n");
    CHECK(same_float(kt_number_to_float(row->number), row->to_float), "toFloat\n");
    CHECK(same_double(kt_number_to_double(row->number), row->to_double), "toDouble\n");
    CHECK(kt_pending_exception() == NULL, "a conversion raised\n");
}

#define INT_MAX_ ((kt_int)0x7fffffff)
#define INT_MIN_ ((kt_int)0x80000000u)
#define LONG_MAX_ ((kt_long)0x7fffffffffffffffll)
#define LONG_MIN_ ((kt_long)0x8000000000000000ull)

void kt_program_entry(void) {
    DRIVER_BEGIN();
    const kt_double nan = __builtin_nan("");
    const kt_float nan_float = __builtin_nanf("");
    Row rows[] = {
        /* NaN 0 0 0 0 NaN NaN */
        {kt_box_double(nan), 0, 0, 0, 0, nan_float, nan},
        /* 1.0E10 -1 -1 2147483647 10000000000 1.0E10 1.0E10 */
        {kt_box_double(1e10), -1, -1, INT_MAX_, 10000000000ll, 1e10f, 1e10},
        /* -1.0E10 0 0 -2147483648 -10000000000 -1.0E10 -1.0E10 */
        {kt_box_double(-1e10), 0, 0, INT_MIN_, -10000000000ll, -1e10f, -1e10},
        /* 3.9 3 3 3 3 3.9 3.9 */
        {kt_box_double(3.9), 3, 3, 3, 3, 3.9f, 3.9},
        /* -3.9 -3 -3 -3 -3 -3.9 -3.9 */
        {kt_box_double(-3.9), -3, -3, -3, -3, -3.9f, -3.9},
        /* 1.0E19 -1 -1 2147483647 9223372036854775807 1.0E19 1.0E19 */
        {kt_box_double(1e19), -1, -1, INT_MAX_, LONG_MAX_, 1e19f, 1e19},
        /* -1.0E19 0 0 -2147483648 -9223372036854775808 -1.0E19 -1.0E19 */
        {kt_box_double(-1e19), 0, 0, INT_MIN_, LONG_MIN_, -1e19f, -1e19},
        /* 4294967297 1 1 1 4294967297 4.2949673E9 4.294967297E9 */
        {kt_box_long(0x100000001ll), 1, 1, 1, 0x100000001ll, 4294967296.0f, 4294967297.0},
        /* 40000 64 -25536 40000 40000 40000.0 40000.0 */
        {kt_box_int(40000), 64, -25536, 40000, 40000, 40000.0f, 40000.0},
        /* 200 -56 200 200 200 200.0 200.0 */
        {kt_box_int(200), -56, 200, 200, 200, 200.0f, 200.0},
        /* 9223372036854775807 -1 -1 -1 9223372036854775807 9.223372E18 9.223372036854776E18 */
        {kt_box_long(LONG_MAX_), -1, -1, -1, LONG_MAX_, 9223372036854775808.0f,
         9223372036854775808.0},
        /* NaN 0 0 0 0 NaN NaN (a Float) */
        {kt_box_float(nan_float), 0, 0, 0, 0, nan_float, nan},
        /* 3.0E9 -1 -1 2147483647 3000000000 3.0E9 3.0E9 (a Float) */
        {kt_box_float(3.0e9f), -1, -1, INT_MAX_, 3000000000ll, 3.0e9f, 3000000000.0},
        /* 1.5 1 1 1 1 1.5 1.5 (a Float) */
        {kt_box_float(1.5f), 1, 1, 1, 1, 1.5f, 1.5},
        /* -5 -5 -5 -5 -5 -5.0 -5.0 (a Byte) */
        {kt_box_byte(-5), -5, -5, -5, -5, -5.0f, -5.0},
        /* 300 44 300 300 300 300.0 300.0 (a Short) */
        {kt_box_short(300), 44, 300, 300, 300, 300.0f, 300.0},
    };
    for (unsigned at = 0; at < sizeof(rows) / sizeof(rows[0]); at++) {
        check(&rows[at]);
    }
    /* `0.1f.toDouble()` widens the float exactly: 0.10000000149011612. */
    CHECK(kt_number_to_double(kt_box_float(0.1f)) == 0.100000001490116119384765625,
          "0.1f.toDouble()\n");
    kt_sys_write(1, "OK\n", 3);
}
