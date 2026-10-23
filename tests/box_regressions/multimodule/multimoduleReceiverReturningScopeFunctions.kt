// MODULE: lib
fun tagged(value: String): String = value.also { }
fun appliedLength(value: String): Int {
    var total = 0
    value.apply { total += length }
    return total
}
// MODULE: main(lib)
fun early(value: String): String {
    value.also { return "OK" }
    return "no non-local return"
}
fun box(): String {
    if (tagged("lib") != "lib") return "library also"
    if (appliedLength("abc") != 3) return "library apply"
    var seen = ""
    val kept = "K".also { seen = it }
    if (kept != "K" || seen != "K") return "also"
    var count = 0
    val applied = "ab".apply { count += length }
    if (applied != "ab" || count != 2) return "apply"
    return early("unused")
}
