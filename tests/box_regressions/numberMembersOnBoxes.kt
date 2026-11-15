// A call of `Number`'s conversions on a value read as a `Number` runs the override of the
// primitive the value is: each one's own rounding, saturation and wrapping, whether the value was
// read as a `Number` before the call site or after it, and through a bounded type parameter.
fun earlier(): Number = 300.toShort()

fun asLong(value: Number): Long = value.toLong()

fun asInt(value: Number): Int = value.toInt()

fun asByte(value: Number): Byte = value.toByte()

fun asShort(value: Number): Short = value.toShort()

fun asFloat(value: Number): Float = value.toFloat()

fun asDouble(value: Number): Double = value.toDouble()

fun <T : Number> bounded(value: T): Int = value.toInt()

fun box(): String {
    if (asLong(earlier()) != 300L) return "short read as a number first"
    if (asLong(-7) != -7L) return "int to long"
    if (asLong(1L shl 40) != 1L shl 40) return "long to long"
    if (asLong(-2.9) != -2L) return "double to long rounds towards zero"
    if (asLong(Float.NaN) != 0L) return "float NaN to long"
    if (asInt(Long.MAX_VALUE) != -1) return "long to int keeps the low bits"
    if (asInt(1e20) != Int.MAX_VALUE) return "double to int saturates"
    if (asInt(-1e20f) != Int.MIN_VALUE) return "float to int saturates"
    if (asInt((-5).toByte()) != -5) return "byte to int"
    if (asByte(200) != (-56).toByte()) return "int to byte wraps"
    if (asShort(70000L) != 4464.toShort()) return "long to short wraps"
    if (asFloat(16777217) != 16777216f) return "int to float rounds"
    if (asDouble(0.1f) != 0.1f.toDouble()) return "float to double"
    if (asDouble(3.toByte()) != 3.0) return "byte to double"
    if (bounded(2.5f) != 2) return "bounded float"
    if (bounded(earlier()) != 300) return "bounded short"
    return "OK"
}
