// Library bodies that loop over an array (`sum`, `average`) iterate it by index, over primitive
// and reference arrays alike, an empty array included.
fun box(): String {
    if (intArrayOf(1, 2, 3, 4).sum() != 10) return "Fail int sum"
    if (intArrayOf().sum() != 0) return "Fail empty sum"
    if (longArrayOf(5L, 6L).sum() != 11L) return "Fail long sum"
    if (byteArrayOf(100, 100).sum() != 200) return "Fail byte sum"
    if (arrayOf(1, 2, 3).sum() != 6) return "Fail boxed sum"
    if (doubleArrayOf(1.0, 2.0).average() != 1.5) return "Fail average"
    return "OK"
}
