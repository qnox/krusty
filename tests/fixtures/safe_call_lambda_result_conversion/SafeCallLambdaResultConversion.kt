class Holder(val text: String, val note: String?)

fun parse(s: String): Int = s.length

fun inferred(holder: Holder) = holder.note?.let { parse(it) }

fun declared(holder: Holder): Int? = holder.note?.let { parse(it) }

fun stored(holder: Holder): Any? {
    val length = holder.note?.let { parse(it) }
    return length
}

fun branch(holder: Holder): Int? = if (holder.text.isEmpty()) null else holder.text.let { parse(it) }

fun appended(holder: Holder): String? = holder.note?.let { it + "!" }

fun widened(holder: Holder): Long? = holder.note?.let { parse(it).toLong() }

fun spread(holder: Holder): Int? = holder.note?.let {
    parse(it)
}

fun box(): String {
    val full = Holder("abc", "de")
    val empty = Holder("", null)
    val result = listOf(
        inferred(full), inferred(empty),
        declared(full), declared(empty),
        stored(full), stored(empty),
        branch(full), branch(empty),
        appended(full), appended(empty),
        widened(full), widened(empty),
        spread(full), spread(empty),
    ).joinToString("|")
    val expected = "2|null|2|null|2|null|3|null|de!|null|2|null|2|null"
    return if (result == expected) "OK" else result
}
