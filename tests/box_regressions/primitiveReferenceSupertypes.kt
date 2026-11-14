// A primitive value reaches a reference supertype, and is compared with a reference `when`
// subject, through its own Kotlin classifier: `kotlin/Int` declares `Number` and `Comparable<Int>`.
fun widest(value: Number): Long = value.toLong()

fun <T : Comparable<T>> larger(a: T, b: T): T = if (a > b) a else b

fun ordered(value: Comparable<Char>): Boolean = value > 'a'

fun kind(subject: Any): String = when (subject) {
    1 -> "one"
    'c' -> "char"
    true -> "bool"
    2L -> "long"
    else -> "other"
}

fun numberKind(subject: Number): String = when (subject) {
    3 -> "three"
    4.0 -> "four"
    else -> "other"
}

fun box(): String {
    if (widest(7) != 7L) return "fail Int as Number"
    if (widest(7.toByte()) != 7L) return "fail Byte as Number"
    if (larger(3, 9) != 9) return "fail Int as Comparable"
    if (!ordered('z')) return "fail Char as Comparable"
    val comparable: Comparable<Int> = 5
    if (comparable.compareTo(4) <= 0) return "fail Int value as Comparable"
    if (kind(1) != "one" || kind('c') != "char" || kind(true) != "bool" || kind(2L) != "long")
        return "fail Any subject"
    if (kind(2) != "other") return "fail Any subject miss"
    if (numberKind(3) != "three" || numberKind(4.0) != "four" || numberKind(3L) != "other")
        return "fail Number subject"
    return "OK"
}
