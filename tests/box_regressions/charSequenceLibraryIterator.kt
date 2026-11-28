// A library function that walks a `CharSequence` hands out the library's own `CharIterator`.
fun box(): String {
    val chars = "ABC".iterator()
    var sum = 0
    while (chars.hasNext()) sum += chars.next().code
    return if (sum == 198) "OK" else "fail sum $sum"
}
