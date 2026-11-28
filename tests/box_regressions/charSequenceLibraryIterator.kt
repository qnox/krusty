// A library function that walks a `CharSequence` hands out the library's own `CharIterator`.
fun box(): String {
    val chars = "ABC".iterator()
    var sum = 0
    while (chars.hasNext()) sum += chars.next().code
    if (sum != 198) return "fail sum $sum"

    val primitive = "AB".iterator()
    if (primitive.nextChar() != 'A' || primitive.nextChar() != 'B' || primitive.hasNext()) return "fail nextChar"

    val boxed: Iterator<Char> = "XY".iterator()
    if (boxed.next() != 'X' || boxed.next() != 'Y' || boxed.hasNext()) return "fail boxed next"
    return "OK"
}
