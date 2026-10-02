// Kotlin's answers for `builder_negative_capacity.c`: `StringBuilder(capacity)` of a negative
// capacity, and of zero and a positive one, which make a builder that grows past its capacity.
fun box(): String = buildString {
    for (capacity in listOf(-1, -42, Int.MIN_VALUE)) {
        val answer = try {
            "made \"${StringBuilder(capacity)}\""
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("StringBuilder($capacity): $answer")
    }
    appendLine("StringBuilder(0): made \"${StringBuilder(0)}\"")
    appendLine("StringBuilder(4) grown: \"${StringBuilder(4).append("past the capacity")}\"")
}
