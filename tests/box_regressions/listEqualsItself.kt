fun equal(a: Any?, b: Any?) = a == b

fun box(): String {
    val xs = ArrayList<Any>()
    xs.add(xs)
    if (!equal(xs, xs)) return "fail self"
    val ys = ArrayList<Any>()
    ys.add("e")
    if (!equal(ys, ys)) return "fail plain"
    return "OK"
}
