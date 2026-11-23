fun pick(t: ((String) -> Boolean)?): Boolean = t?.invoke("a") ?: false
fun pickDefault(t: ((String) -> Boolean)? = null): Boolean = t?.invoke("ab") ?: false
fun pickAfter(n: Int, t: ((String) -> Int)?): Int = n + (t?.invoke("abc") ?: 0)
fun pickUnit(t: ((String) -> Unit)?): Boolean {
    t?.invoke("x")
    return t != null
}

fun box(): String {
    if (!pick { it.length == 1 }) return "fail trailing implicit it"
    if (!pick(t = { it.length == 1 })) return "fail named implicit it"
    if (!pick({ s -> s.length == 1 })) return "fail declared name"
    if (!pick({ s: String -> s.length == 1 })) return "fail declared type"
    if (!pickDefault { it.length == 2 }) return "fail default parameter"
    if (pickDefault()) return "fail omitted default"
    if (pickAfter(1) { it.length } != 4) return "fail leading argument"
    if (!pickUnit { it.length }) return "fail unit result"
    return "OK"
}
