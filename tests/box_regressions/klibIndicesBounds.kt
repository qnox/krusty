// A counted `for` loop over the `indices` of a `CharSequence` or a `Collection`, and the stdlib
// body that loops over a text's `indices` (`trimEnd`).
fun box(): String {
    if ("abc  ".trimEnd() != "abc") return "fail trailing"
    if ("  abc".trimEnd() != "  abc") return "fail leading kept"
    if ("   ".trimEnd() != "") return "fail blank"
    if ("".trimEnd() != "") return "fail empty"
    val list: Collection<Int> = listOf(4, 5, 6)
    var sum = 0
    for (i in list.indices) sum += i
    if (sum != 3) return "fail collection indices"
    var count = 0
    for (i in emptyList<Int>().indices) count++
    if (count != 0) return "fail empty indices"
    return "OK"
}
