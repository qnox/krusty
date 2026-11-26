enum class Mode { A, B, C }

open class Holder {
    enum class Kind { X, Y, Z }
    val kind: Kind? = Kind.Y
}

fun viaLocal(value: Any, mode: Mode?): Boolean = when (value) {
    is String -> {
        val m = mode
        m == Mode.A || m == Mode.B
    }
    else -> false
}

fun viaProperty(value: Any): Boolean = when (value) {
    is Holder -> {
        val k = value.kind
        k == Holder.Kind.X || k == Holder.Kind.Y
    }
    else -> false
}

fun subject(mode: Mode?): String = when (mode) {
    Mode.A, Mode.B -> "ab"
    Mode.C -> "c"
    null -> "null"
}

fun mixed(value: Any, mode: Mode?): String {
    val inside = when (value) {
        is String -> {
            val m = mode
            m == Mode.C && m != Mode.A
        }
        else -> false
    }
    return subject(mode) + inside
}

fun box(): String {
    if (!viaLocal("s", Mode.B) || viaLocal("s", Mode.C) || viaLocal(1, Mode.A)) return "local"
    if (!viaProperty(Holder()) || viaProperty("x")) return "property"
    if (mixed("s", Mode.C) != "ctrue" || mixed("s", null) != "nullfalse") return "mixed"
    return "OK"
}
