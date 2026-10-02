// Kotlin's answers for `array_list_negative_capacity.c`: what `ArrayList(capacity)` answers, and
// the list after taking an element when it answers one.
fun box(): String = buildString {
    fun capacity(label: String, capacity: Int) {
        val answer = try {
            val list = ArrayList<Any?>(capacity)
            list.add(1)
            "$list"
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}: ${e.message}"
        }
        appendLine("$label: $answer")
    }
    capacity("ArrayList(-1)", -1)
    capacity("ArrayList(-2147483648)", Int.MIN_VALUE)
    capacity("ArrayList(0)", 0)
    capacity("ArrayList(4)", 4)
}
