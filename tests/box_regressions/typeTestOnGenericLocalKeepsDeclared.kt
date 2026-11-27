// `is IntBox` on a stable value of type `Source<T>` keeps the declaration beside the proof:
// `IntBox` is a `Source<Int>`, not a `Source<T>`, so the value is `Source<T> & IntBox`. A local,
// a parameter and `this` follow the one rule: `value` dispatches through `IntBox` as an `Int`,
// and members of either constituent stay visible.
interface Source<out T> {
    val value: T
    val label: String get() = "source"

    fun echo(): String {
        if (this is IntBox) return label + ":" + (value + 1) + ":" + doubled()
        return label
    }
}

class IntBox(override val value: Int) : Source<Int> {
    fun doubled(): Int = value * 2
}

class TextBox(override val value: String) : Source<String>

fun <T> parameter(source: Source<T>): String {
    if (source is IntBox) return source.label + ":" + (source.value + 1) + ":" + source.doubled()
    return source.label
}

fun <T> local(make: () -> Source<T>): String {
    val source = make()
    if (source is IntBox) return source.label + ":" + (source.value + 1) + ":" + source.doubled()
    return source.label
}

fun box(): String {
    if (parameter(IntBox(4)) != "source:5:8") return "parameter int: " + parameter(IntBox(4))
    if (parameter(TextBox("t")) != "source") return "parameter text"
    if (local { IntBox(4) } != "source:5:8") return "local int: " + local { IntBox(4) }
    if (local { TextBox("t") } != "source") return "local text"
    if (IntBox(4).echo() != "source:5:8") return "this int: " + IntBox(4).echo()
    if (TextBox("t").echo() != "source") return "this text"
    return "OK"
}
