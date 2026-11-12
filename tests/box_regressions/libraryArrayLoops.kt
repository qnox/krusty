// Library bodies that loop over an array (`sum`, `average`, `any`, `all`) iterate it by index,
// over primitive and reference arrays alike, an empty array included, and may leave the loop
// before its last element.
fun box(): String {
    if (intArrayOf(1, 2, 3, 4).sum() != 10) return "Fail int sum"
    if (intArrayOf().sum() != 0) return "Fail empty sum"
    if (longArrayOf(5L, 6L).sum() != 11L) return "Fail long sum"
    if (byteArrayOf(100, 100).sum() != 200) return "Fail byte sum"
    if (arrayOf(1, 2, 3).sum() != 6) return "Fail boxed sum"
    if (doubleArrayOf(1.0, 2.0).average() != 1.5) return "Fail average"
    var visited = 0
    if (!intArrayOf(1, 2, 3).any { visited++; it == 2 } || visited != 2) return "Fail any $visited"
    if (arrayOf("a", "b").all { it == "a" }) return "Fail all"
    if (!intArrayOf().all { false }) return "Fail empty all"
    return "OK"
}
