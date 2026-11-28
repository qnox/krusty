// A library inline body that walks a `CharSequence` iterates the library's own `CharIterator`.
fun box(): String {
    val sum = "ABC".sumOf { it.code }
    if (sum != 198) return "fail sum $sum"
    var chars = ""
    for (c in "xyz") chars += c
    if (chars != "xyz") return "fail walk $chars"
    return "OK"
}
