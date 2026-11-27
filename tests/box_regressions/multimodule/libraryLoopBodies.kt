// A library's top-level functions loop with `while`, `do`-`while` and `for`, over ranges, progressions
// and arrays, leave and resume loops with `break` and `continue`, labeled ones included. A program
// linked against the library's KLIB runs each body as the library wrote it.
// MODULE: lib
// FILE: Library.kt
package fixture

fun sumBelow(limit: Int): Int {
    var total = 0
    var i = 0
    while (i < limit) {
        i = i + 1
        if (i == 3) continue
        if (i > 8) break
        total = total + i
    }
    return total
}

fun firstPowerAtLeast(bound: Int): Int {
    var power = 1
    do {
        power = power * 2
    } while (power < bound)
    return power
}

fun sumUntil(n: Int): Int {
    var total = 0
    for (i in 0 until n) total = total + i
    return total
}

fun sumClosed(from: Int, to: Int): Int {
    var total = 0
    for (i in from..to) {
        total = total + i
    }
    return total
}

fun countDown(from: Int): String {
    var digits = ""
    for (i in from downTo 1) digits = "$digits$i"
    return digits
}

fun sumArray(values: IntArray): Int {
    var total = 0
    for (value in values) total = total + value
    return total
}

fun joined(words: Array<String>): String {
    var text = ""
    for (word in words) {
        if (word == "skip") continue
        text = text + word
    }
    return text
}

fun firstPair(n: Int): String {
    var found = "none"
    outer@ for (a in 1..n) {
        for (b in 1..n) {
            if (a * b == 6) {
                found = "$a*$b"
                break@outer
            }
            if (b > a) continue@outer
        }
    }
    return found
}

// MODULE: main(lib)
// FILE: Main.kt
import fixture.*

fun box(): String {
    if (sumBelow(5) != 12) return "sumBelow 5"
    if (sumBelow(20) != 33) return "sumBelow 20"
    if (sumBelow(0) != 0) return "sumBelow 0"
    if (firstPowerAtLeast(0) != 2) return "firstPowerAtLeast 0"
    if (firstPowerAtLeast(100) != 128) return "firstPowerAtLeast 100"
    if (sumUntil(5) != 10) return "sumUntil"
    if (sumUntil(0) != 0) return "sumUntil 0"
    if (sumClosed(3, 5) != 12) return "sumClosed"
    if (sumClosed(5, 3) != 0) return "sumClosed empty"
    if (countDown(5) != "54321") return "countDown"
    if (sumArray(intArrayOf(1, 2, 3, 4)) != 10) return "sumArray"
    if (joined(arrayOf("a", "skip", "b")) != "ab") return "joined"
    if (firstPair(3) != "2*3") return "firstPair 3"
    if (firstPair(2) != "none") return "firstPair 2"
    return "OK"
}
