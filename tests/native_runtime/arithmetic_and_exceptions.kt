// Kotlin's answers for `arithmetic_and_exceptions.c`: Kotlin's integer arithmetic where C's differs
// or is undefined, and the exceptions and wording a failing operation or assertion reports.
import kotlin.math.abs
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

fun zero() = 0

fun box(): String {
    val out = StringBuilder()
    fun line(label: String, value: Any?) = out.append("$label = $value\n")
    fun raised(label: String, block: () -> Any?) {
        val answer = try {
            block().toString()
        } catch (e: Throwable) {
            "${e::class.simpleName}: ${e.message}"
        }
        line(label, answer)
    }
    line("Int.MIN_VALUE / -1", Int.MIN_VALUE / -1)
    line("Long.MIN_VALUE / -1", Long.MIN_VALUE / -1)
    line("-7 % 2", -7 % 2)
    line("7 % -2", 7 % -2)
    line("Int.MIN_VALUE % -1", Int.MIN_VALUE % -1)
    line("(-7).mod(2)", (-7).mod(2))
    line("7.mod(-2)", 7.mod(-2))
    line("Int.MIN_VALUE.mod(3)", Int.MIN_VALUE.mod(3))
    line("5.mod(Int.MIN_VALUE)", 5.mod(Int.MIN_VALUE))
    line("Long.MIN_VALUE.mod(3L)", Long.MIN_VALUE.mod(3L))
    line("1 shl 32", 1 shl 32)
    line("1 shl -1", 1 shl -1)
    line("Int.MIN_VALUE shr 32", Int.MIN_VALUE shr 32)
    line("-8 shr 1", -8 shr 1)
    line("-1 ushr 28", -1 ushr 28)
    line("-1L ushr 64", -1L ushr 64)
    line("Long.MIN_VALUE shr 63", Long.MIN_VALUE shr 63)
    line("abs(Int.MIN_VALUE)", abs(Int.MIN_VALUE))
    line("abs(-5L)", abs(-5L))
    line("abs(-0.0).toRawBits()", abs(-0.0).toRawBits())
    line("ULong.MAX_VALUE / 2uL", ULong.MAX_VALUE / 2uL)
    line("(1uL shl 63) % 3uL", (1uL shl 63) % 3uL)
    line("(UInt.MAX_VALUE - 1u) / 2u", (UInt.MAX_VALUE - 1u) / 2u)
    line("ULong.MAX_VALUE", ULong.MAX_VALUE)
    line("(-128).toUByte()", (-128).toUByte())
    line("UInt.MAX_VALUE", UInt.MAX_VALUE)
    line("(-0.0).compareTo(0.0)", (-0.0).compareTo(0.0))
    line("NaN.compareTo(POSITIVE_INFINITY)", Double.NaN.compareTo(Double.POSITIVE_INFINITY))
    line("NaN.compareTo(NaN)", Double.NaN.compareTo(Double.NaN))
    line("(-0.0f).compareTo(0.0f)", (-0.0f).compareTo(0.0f))
    raised("1 / 0") { 1 / zero() }
    try {
        1 / zero()
    } catch (e: Throwable) {
        line("1 / 0 is ArithmeticException", e is ArithmeticException)
        line("1 / 0 is RuntimeException", e is RuntimeException)
        line("1 / 0 is Error", e is Error)
    }
    raised("1uL % 0uL") { 1uL % zero().toULong() }
    val bare = IllegalStateException()
    line("IllegalStateException()", "${bare::class.simpleName}: ${bare.message}")
    line("RuntimeException(bare).cause === bare", RuntimeException(bare).cause === bare)
    line("RuntimeException(bare).message", RuntimeException(bare).message)
    raised("assertEquals(1, 2)") { assertEquals(1, 2) }
    raised("assertTrue(false, \"m\")") { assertTrue(false, "m") }
    raised("assertEquals(1, 1)") { assertEquals(1, 1) }
    raised("assertFailsWith<IllegalStateException> completing") {
        assertFailsWith<IllegalStateException> { }
    }
    raised("assertFailsWith<IllegalStateException>(\"m\") throwing") {
        assertFailsWith<IllegalStateException>("m") { throw ArithmeticException("/ by zero") }
    }
    return out.toString()
}
