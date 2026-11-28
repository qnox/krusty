// kotlinc keeps the `StringBuilder` of a concatenation with at least 23 parts in a local and
// appends to it in chains of 23, so a long template, a long `+` chain, and one with an inline call
// inside a chain all build the same text as a short one. 22 parts stay a single chain.
fun parts(n: Int, x: Int): String = when (n) {
    22 -> "${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}"
    23 -> "${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}"
    24 -> "${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}"
    46 -> "${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}"
    47 -> "${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}${x}"
    else -> ""
}

fun mixed(i: Int, l: Long, c: Char, s: String?, d: Double): String =
    "<$i,$l,$c,$s,$d><$i,$l,$c,$s,$d><$i,$l,$c,$s,$d><$i,$l,$c,$s,$d><$i,$l,$c,$s,$d><$i,$l,$c,$s,$d>"

fun chained(x: Int): String =
    "p0=${x}" +
        "p1=${x}" +
        "p2=${x}" +
        "p3=${x}" +
        "p4=${x}" +
        "p5=${x}" +
        "p6=${x}" +
        "p7=${x}" +
        "p8=${x}" +
        "p9=${x}" +
        "p10=${x}" +
        "p11=${x}" +
        "p12=${x}" +
        "p13=${x}"

fun inlined(xs: Array<String>): String =
    "[${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}]" +
        ",let=${xs.let { it.size }}" +
        "[${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}][${xs.size}]"

fun box(): String {
    for (n in listOf(22, 23, 24, 46, 47)) {
        val text = parts(n, 7)
        if (text != "7".repeat(n)) return "FAIL $n: $text"
    }
    val mixed = mixed(1, 2L, 'c', null, 0.5)
    if (mixed != "<1,2,c,null,0.5>".repeat(6)) return "FAIL mixed: $mixed"
    val chained = chained(3)
    if (chained != (0 until 14).joinToString("") { "p$it=3" }) return "FAIL chained: $chained"
    val inlined = inlined(arrayOf("a", "b"))
    if (inlined != "[2]".repeat(11) + ",let=2" + "[2]".repeat(11)) return "FAIL inlined: $inlined"
    return "OK"
}
