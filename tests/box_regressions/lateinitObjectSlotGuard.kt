// An uninitialized `lateinit` read of a property whose storage is erased to `Object` (`Any`, an
// unbounded type parameter) throws naming the property, from its getter and from a raw private
// read alike; a narrower reference (`CharSequence`) throws the same way.
class Slots<T : Any> {
    lateinit var any: Any
    private lateinit var hidden: Any
    lateinit var generic: T
    lateinit var text: CharSequence

    fun hiddenText(): String = hidden.toString()
    fun hide(value: Any) {
        hidden = value
    }
}

fun failure(read: () -> Any): String =
    try {
        "read ${read()}"
    } catch (e: RuntimeException) {
        e.message ?: "no message"
    }

fun box(): String {
    val slots = Slots<Any>()
    val expected = listOf("any", "hidden", "generic", "text").map {
        "lateinit property $it has not been initialized"
    }
    val actual = listOf(
        failure { slots.any },
        failure { slots.hiddenText() },
        failure { slots.generic },
        failure { slots.text },
    )
    if (actual != expected) return "FAIL: $actual"
    slots.any = "O"
    slots.hide("K")
    slots.generic = "!"
    slots.text = "?"
    if ("${slots.generic}${slots.text}" != "!?") return "FAIL: ${slots.generic}${slots.text}"
    return slots.any.toString() + slots.hiddenText()
}
