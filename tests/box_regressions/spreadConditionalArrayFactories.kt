// A spread operand is the vararg's whole array, so a conditional spread gives each branch the
// vararg's array as its expectation: the empty `arrayOf()` branches infer `String` from
// `vararg xs: String`, as a direct `*arrayOf()` does (kotlinc 2.4.21).
fun take(vararg xs: String): Int = xs.size

fun box(): String {
    val yes = true
    val no = false
    val fromIf = take("a", *if (yes) arrayOf("x") else arrayOf())
    val emptyIf = take("a", *if (no) arrayOf("x") else arrayOf())
    val bothEmpty = take(*if (yes) arrayOf() else arrayOf())
    val fromWhen = take(*when { no -> arrayOf("x"); else -> arrayOf() }, "b")
    val counts = "$fromIf $emptyIf $bothEmpty $fromWhen"
    return if (counts == "2 1 0 1") "OK" else "FAIL: $counts"
}
