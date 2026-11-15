// WITH_STDLIB
// A dependency's bodies count through range literals, compare scalars through `compareTo`,
// convert scalars, and initialize a `val` after its declaration. kotlinc's `ForLoopsLowering`
// counts each range literal instead of iterating the progression it builds.
// MODULE: lib
// FILE: Library.kt
package fixture

fun parseDigits(chars: CharArray, radix: Int): Int {
    val sign: Int
    val start: Int
    if (chars.isNotEmpty() && chars[0] == '-') {
        sign = -1
        start = 1
    } else {
        sign = 1
        start = 0
    }
    var result = 0
    for (index in start until chars.size) {
        val char = chars[index]
        if (char < '0' || char > '9') return Int.MIN_VALUE
        result = result * radix + (char.code - '0'.code)
    }
    return sign * result
}

fun countDown(from: Int, to: Int): Int {
    var sum = 0
    for (value in from downTo to) sum = sum * 10 + value
    return sum
}

fun closedSum(from: Long, to: Long): Long {
    var sum = 0L
    for (value in from..to) sum += value
    return sum
}

fun openCount(from: Char, to: Char): Int {
    var count = 0
    for (char in from..<to) count += 1
    return count
}

fun breakAt(limit: Int, stop: Int): Int {
    var last = -1
    for (value in 0..limit) {
        if (value == stop) break
        if (value % 2 == 0) continue
        last = value
    }
    return last
}

fun narrow(value: Long): Int = value.toInt()

fun order(left: Long, right: Long): Int = left.compareTo(right)

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (parseDigits(charArrayOf('4', '2'), 10) != 42) return "digits"
    if (parseDigits(charArrayOf('-', '7'), 10) != -7) return "negative"
    if (parseDigits(charArrayOf('x'), 10) != Int.MIN_VALUE) return "invalid"
    if (parseDigits(charArrayOf(), 10) != 0) return "empty"
    if (countDown(3, 1) != 321) return "downTo"
    if (countDown(1, 3) != 0) return "empty downTo"
    if (closedSum(1L, 4L) != 10L) return "closed"
    if (openCount('a', 'e') != 4) return "open"
    if (openCount('e', 'a') != 0) return "empty open"
    if (breakAt(10, 6) != 5) return "jumps"
    if (narrow(0x1_0000_0002L) != 2) return "narrow"
    if (order(1L, 2L) >= 0 || order(2L, 1L) <= 0 || order(3L, 3L) != 0) return "order"
    return "OK"
}
