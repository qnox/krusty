// Kotlin's answers for `string_get_negative_index.c`: `"abc"[index]` inside the text, past its end
// and at negative indices.
fun box(): String = buildString {
    val text = listOf("abc")[0]
    for (index in listOf(-1, -100, Int.MIN_VALUE, 0, 2, 3)) {
        val answer = try {
            "answered ${text[index]}"
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("\"abc\"[$index]: $answer")
    }
}
