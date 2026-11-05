// A primitive `when` comparand meets a reference subject as its own Kotlin classifier:
// `kotlin/Int` declares `Number` and `Comparable<Int>` on every platform, whatever boxes it.
fun numberKind(subject: Number): String = when (subject) {
    3 -> "three"
    4.0 -> "four"
    else -> "other"
}

fun comparableKind(subject: Comparable<Int>): String = when (subject) {
    1 -> "one"
    else -> "other"
}

fun anyKind(subject: Any): String = when (subject) {
    1 -> "one"
    'c' -> "char"
    true -> "bool"
    2L -> "long"
    else -> "other"
}

fun box(): String {
    if (numberKind(3) != "three" || numberKind(4.0) != "four" || numberKind(3L) != "other")
        return "fail Number subject"
    if (comparableKind(1) != "one" || comparableKind(2) != "other") return "fail Comparable subject"
    if (anyKind(1) != "one" || anyKind('c') != "char" || anyKind(true) != "bool") return "fail Any"
    if (anyKind(2L) != "long" || anyKind(2) != "other") return "fail Any miss"
    return "OK"
}
