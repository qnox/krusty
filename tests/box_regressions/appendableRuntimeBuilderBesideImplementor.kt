// A `StringBuilder` and a class of the program both appended to through one `Appendable` call
// site: each keeps its own `append`, whichever of them the target makes.

class Counter : Appendable {
    var count = 0
    override fun append(value: Char): Appendable { count += 1; return this }
    override fun append(value: CharSequence?): Appendable { count += 10; return this }
    override fun append(value: CharSequence?, startIndex: Int, endIndex: Int): Appendable {
        count += 100
        return this
    }
}

fun put(a: Appendable) {
    a.append('y')
    a.append("zz")
}

fun box(): String {
    val sb = StringBuilder()
    put(sb)
    if (sb.toString() != "yzz") return "fail builder: $sb"
    val counter = Counter()
    put(counter)
    if (counter.count != 11) return "fail counter: ${counter.count}"
    return "OK"
}
