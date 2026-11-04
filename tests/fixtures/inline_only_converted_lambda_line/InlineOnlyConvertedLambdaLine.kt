class Label(val text: String)

fun nullable(s: String): String? = s.let { it.trim() }

fun listed(s: String): List<String>? = s.let { listOf(it) }

fun anything(s: String): Any? = s.let { it.length.toString() }

fun labelled(s: String): Label? = s.run { Label(this) }

fun spread(s: String): String? = s.let {
    it.trim()
}

fun box(): String {
    val result = listOf(
        nullable(" a "),
        listed("b").toString(),
        anything("cc"),
        labelled("d")?.text,
        spread(" e "),
    ).joinToString("|")
    return if (result == "a|[b]|2|d|e") "OK" else result
}
