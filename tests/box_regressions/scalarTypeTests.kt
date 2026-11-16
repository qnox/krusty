// Type tests and safe casts of an `Any?` against scalar types: each scalar classifier is its own
// type, so a `Char` is not an `Int`, a `UInt` is not an `Int`, and a `Long` is not an `Int`. A
// `null` and a value of an untested type match none of them.

fun kind(x: Any?): String = when (x) {
    is Char -> "c"
    is Int -> "i"
    is Long -> "l"
    is Boolean -> "b"
    is UInt -> "u"
    is Double -> "d"
    is String -> "s"
    else -> "-"
}

fun box(): String {
    val kinds = kind('a') + kind(1) + kind(2L) + kind(true) + kind(3u) + kind(1.5) + kind("x") +
        kind(null) + kind(1.5f)
    if (kinds != "cilbuds--") return "FAIL kinds: $kinds"
    val c: Any? = 'z'
    if (c is Int || c !is Char) return "FAIL is"
    if ((c as? Int) != null || (c as? Char) != 'z') return "FAIL safe cast"
    val n: Any? = null
    if (n is Char) return "FAIL null"
    return "OK"
}
