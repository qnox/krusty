// MODULE: lib
fun inlineBehavior(): String {
    val big = (-1).toUInt()
    if (big / 2u != 2147483647u) return "fail: div ${big / 2u}"
    if (big % 10u != 5u) return "fail: rem ${big % 10u}"
    if (big.toLong() != 4294967295L) return "fail: widen ${big.toLong()}"
    return "OK"
}
// MODULE: main(lib)
fun box(): String = inlineBehavior()
