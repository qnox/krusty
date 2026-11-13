@JvmInline value class HeaderInt(val value: Int)
@JvmInline value class HeaderLong(val value: Long)
@JvmInline value class HeaderText(val value: String)

fun checkHeaderPayload(first: Any, same: Any, other: Any, expectedHash: Int): String {
    if (first != same || same != first) return "equal payloads differ"
    if (first == other || other == first) return "different payloads equal"
    if (first.hashCode() != expectedHash || same.hashCode() != expectedHash) return "payload hash"
    return "OK"
}

fun box(): String {
    val intResult = checkHeaderPayload(HeaderInt(7), HeaderInt(7), HeaderInt(8), 7)
    if (intResult != "OK") return "Int: $intResult"
    val longResult = checkHeaderPayload(HeaderLong(4294967296L), HeaderLong(4294967296L), HeaderLong(4294967297L), 1)
    if (longResult != "OK") return "Long: $longResult"
    val textResult = checkHeaderPayload(HeaderText("A"), HeaderText("A"), HeaderText("B"), 65)
    if (textResult != "OK") return "String: $textResult"
    return "OK"
}
