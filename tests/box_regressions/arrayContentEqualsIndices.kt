// `contentEquals` loops over `this.indices`. A KLIB serializes that loop as the iterator protocol
// over the extension property's result; kotlinc counts it from zero until the receiver's size.
fun box(): String {
    var results = ""
    results += intArrayOf(1, 2, 3).contentEquals(intArrayOf(1, 2, 3))
    results += intArrayOf(1, 2, 3).contentEquals(intArrayOf(1, 2, 4))
    results += intArrayOf(1, 2, 3).contentEquals(intArrayOf(1, 2))
    results += longArrayOf(1L, 2L).contentEquals(longArrayOf(1L, 2L))
    results += byteArrayOf(1, 2).contentEquals(byteArrayOf(1, 3))
    results += charArrayOf('a', 'b').contentEquals(charArrayOf('a', 'b'))
    results += booleanArrayOf(true, false).contentEquals(booleanArrayOf(true, true))
    results += intArrayOf().contentEquals(intArrayOf())
    val expected = "truefalsefalsetruefalsetruefalsetrue"
    return if (results == expected) "OK" else results
}
