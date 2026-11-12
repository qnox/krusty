enum class Shade { LIGHT, DARK }

fun sink(v: Any?) {}

fun names(): Int {
    var n = 0
    for (s in Shade.values()) {
        n += s.ordinal
    }
    return n
}

fun text(x: UInt): String {
    val s = (x + 1u).toString()
    return s
}

fun late(flag: Boolean): Int {
    var k: Int
    if (flag) {
        k = 1
    } else {
        k = 2
    }
    return k
}

fun branch(flag: Boolean) {
    if (flag) sink(1)
    sink(2)
}

fun runs(): Int {
    val v = run {
        sink(3)
        4
    }
    return v
}

fun box(): String {
    if (names() != 1) return "names"
    if (text(4294967294u) != "4294967295") return "text"
    if (late(true) != 1) return "late true"
    if (late(false) != 2) return "late false"
    branch(true)
    branch(false)
    if (runs() != 4) return "runs"
    return "OK"
}
