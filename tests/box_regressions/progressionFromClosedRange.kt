// The progression classes' `(start, endInclusive, step)` constructor, reached through the public
// `fromClosedRange` factories: the last element is normalized onto the step, and a zero or minimum
// step is Kotlin's IllegalArgumentException with its own message.

fun failure(block: () -> Any): String =
    try {
        "built ${block()}"
    } catch (e: IllegalArgumentException) {
        e.message ?: "no message"
    }

fun walk(progression: IntProgression): String {
    var walked = ""
    for (element in progression) walked += "$element,"
    return walked
}

fun box(): String {
    val ints = IntProgression.fromClosedRange(1, 10, 3)
    if (ints.first != 1 || ints.last != 10 || ints.step != 3) return "ints: $ints"
    val down = IntProgression.fromClosedRange(10, 1, -4)
    if (walk(down) != "10,6,2," || down.last != 2) return "down: ${walk(down)}"
    val longs = LongProgression.fromClosedRange(0L, 9L, 4L)
    if (longs.first != 0L || longs.last != 8L || longs.step != 4L) return "longs: $longs"
    val chars = CharProgression.fromClosedRange('a', 'f', 2)
    if (chars.first != 'a' || chars.last != 'e' || chars.step != 2) return "chars: $chars"
    val empty = IntProgression.fromClosedRange(5, 1, 1)
    if (!empty.isEmpty()) return "empty: $empty"

    val zero = failure { IntProgression.fromClosedRange(0, 1, 0) }
    if (zero != "Step must be non-zero.") return "zero: $zero"
    val intMin = failure { IntProgression.fromClosedRange(0, 1, Int.MIN_VALUE) }
    if (intMin != "Step must be greater than Int.MIN_VALUE to avoid overflow on negation.") {
        return "intMin: $intMin"
    }
    val longMin = failure { LongProgression.fromClosedRange(0L, 1L, Long.MIN_VALUE) }
    if (longMin != "Step must be greater than Long.MIN_VALUE to avoid overflow on negation.") {
        return "longMin: $longMin"
    }
    val charZero = failure { CharProgression.fromClosedRange('a', 'b', 0) }
    if (charZero != "Step must be non-zero.") return "charZero: $charZero"
    return "OK"
}
