// MODULE: lib
// kotlinc lets a file that suppresses INVISIBLE_REFERENCE and INVISIBLE_MEMBER call a dependency's
// internal extensions through an import, defaulted parameters included.
package okio.internal

internal fun IntArray.commonSum(beginIndex: Int = 0, endIndex: Int = size): Int {
    var sum = 0
    for (index in beginIndex until endIndex) sum += this[index]
    return sum
}

internal fun String.commonDoubledLength(): Int = length * 2

// MODULE: main(lib)
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

import okio.internal.commonDoubledLength
import okio.internal.commonSum

fun IntArray.total(): Int = commonSum()

fun box(): String {
    val values = intArrayOf(1, 2, 3, "OK".commonDoubledLength())
    if (values.total() != 10) return "total: ${values.total()}"
    if (values.commonSum(1, 3) != 5) return "range: ${values.commonSum(1, 3)}"
    return "OK"
}
