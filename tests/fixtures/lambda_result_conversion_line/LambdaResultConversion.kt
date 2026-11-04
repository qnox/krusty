fun parse(s: String): Int = s.length

fun boxed(s: String): Int? = s.let { parse(it) }

fun anyResult(s: String): Any = s.let { parse(it) }

fun received(s: String): Any? = s.run { parse(this) }

fun spread(s: String): Any = s.let {
    parse(it)
}

fun wide(s: String): Long? = s.let { parse(it).toLong() }

fun box(): String {
    val result = listOf(
        boxed("ab"),
        anyResult("abc"),
        received("a"),
        spread("abcd"),
        wide("abcde"),
    ).joinToString("|")
    return if (result == "2|3|1|4|5") "OK" else result
}
