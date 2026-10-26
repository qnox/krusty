// WITH_STDLIB
// `toString(radix)` reaches the stdlib's `checkRadix`, whose body tests `radix !in MIN..MAX`; a
// range literal's membership in a library body is a comparison with its bounds.
fun lo() = 2
fun hi() = 36

fun box(): String {
    val r = lo()..hi()
    if (!r.contains(16)) return "fail1"
    if (40 in r) return "fail2"
    if (255.toString(16) != "ff") return "fail3"
    if ((-255L).toString(2) != "-11111111") return "fail4"
    if (Long.MIN_VALUE.toString(36) != "-1y2p0ij32e8e8") return "fail5"
    return "OK"
}
