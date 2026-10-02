// Kotlin's answers for `string_substring_bounds.c`: `substring` of `"abc"` with bounds outside the
// text, and inside it.
fun box(): String = buildString {
    val text = listOf("abc")[0]
    fun t(label: String, slice: () -> String) {
        val answer = try {
            "answered \"${slice()}\""
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label: $answer")
    }
    t("substring(-1, 2)") { text.substring(-1, 2) }
    t("substring(-1)") { text.substring(-1) }
    t("substring(2, 1)") { text.substring(2, 1) }
    t("substring(2, 10)") { text.substring(2, 10) }
    t("substring(4)") { text.substring(4) }
    t("substring(1, 3)") { text.substring(1, 3) }
    t("substring(3)") { text.substring(3) }
}
