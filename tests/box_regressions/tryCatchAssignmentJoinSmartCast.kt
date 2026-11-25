class Holder { val items: Array<String> = arrayOf("x") }

class TooBig : Exception()

fun use(a: Array<String>): Int = a.size

fun tryCatchEveryEdge(): Int {
    var items: Array<String>?
    try {
        items = Holder().items
    } catch (_: TooBig) {
        items = Holder().items
    }
    return use(items)
}

fun nestedInElse(flag: Boolean): Int {
    var items: Array<String>?
    if (flag) {
        items = Holder().items
    } else {
        try {
            items = Holder().items
        } catch (_: TooBig) {
            items = Holder().items
        }
    }
    return use(items)
}

fun box(): String {
    if (tryCatchEveryEdge() != 1) return "fail: try/catch"
    if (nestedInElse(false) != 1 || nestedInElse(true) != 1) return "fail: nested"
    return "OK"
}
