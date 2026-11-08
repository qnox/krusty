class Record(val id: String, val note: String?, val label: String?)

class Summary(val id: String, val note: String?, val label: String?)

fun shout(text: String): String = text.uppercase()

fun pairOf(record: Record) =
    Pair(
        record.id,
        record.note?.let { shout(it) },
    )

fun summaryOf(record: Record) =
    Summary(
        id = record.id,
        note = record.note?.let { shout(it) },
        label = record.label,
    )

fun listed(record: Record): List<String?> =
    listOf(
        record.id,
        record.note?.let { shout(it) },
    )

fun direct(record: Record) = record.note?.let { shout(it) }

fun render(summary: Summary) = "${summary.id}:${summary.note}:${summary.label}"

fun box(): String {
    val full = Record("a", "b", "c")
    val empty = Record("x", null, null)
    val result = listOf(
        pairOf(full).toString(),
        pairOf(empty).toString(),
        render(summaryOf(full)),
        render(summaryOf(empty)),
        listed(full).toString(),
        listed(empty).toString(),
        direct(full).toString(),
        direct(empty).toString(),
    ).joinToString("|")
    val expected = "(a, B)|(x, null)|a:B:c|x:null:null|[a, B]|[x, null]|B|null"
    return if (result == expected) "OK" else result
}
