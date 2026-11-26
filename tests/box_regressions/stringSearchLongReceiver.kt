// WITH_STDLIB

fun box(): String {
    val builder = StringBuilder()
    var count = 0
    while (count < 300_000) {
        builder.append('a')
        count++
    }
    val long = builder.toString()
    if (long.indexOf("z") != -1) return "FAIL absent needle: ${long.indexOf("z")}"
    if (long.indexOf("az") != -1) return "FAIL absent two-unit needle"
    val tail = long + "bz"
    if (tail.indexOf("bz") != 300_000) return "FAIL late needle: ${tail.indexOf("bz")}"
    if (tail.indexOf("a", 299_999) != 299_999) return "FAIL late start: ${tail.indexOf("a", 299_999)}"
    val pairs = "x😀y😀"
    if (pairs.indexOf("😀") != 1) return "FAIL pair: ${pairs.indexOf("😀")}"
    if (pairs.indexOf("😀", 2) != 4) return "FAIL pair from: ${pairs.indexOf("😀", 2)}"
    if (pairs.indexOf("y", 3) != 3) return "FAIL after pair: ${pairs.indexOf("y", 3)}"
    return "OK"
}
