// A reference array of a scalar element stores each element as its box: `Array<Int>` and
// `Array<Char?>` are written and read back as values, compare by value through `Any`, and keep
// boxes when spread into an `Array<Any>`.
fun sum(values: Array<Int>): Int {
    var total = 0
    for (value in values) total += value
    return total
}

fun box(): String {
    val ints = arrayOf(1, 2, 3)
    ints[1] = 20
    if (sum(ints) != 24) return "sum ${sum(ints)}"
    val doubles = arrayOf(0.5, 1.5)
    if (doubles[0] + doubles[1] != 2.0) return "doubles"
    val chars: Array<Char?> = arrayOf('O', null, 'K')
    if (chars[1] != null || chars.size != 3) return "nullable"
    val any: Array<Any> = arrayOf(*ints, 'c', 4L)
    if (any[1] != 20 || any[3] != 'c' || any[4] != 4L || any[4] == 4) return "spread"
    return "" + chars[0] + chars[2]
}
