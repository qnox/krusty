// `this is IntBox` on a receiver `Source<T>` keeps `Source<T> & IntBox`: the bare `value` is
// both `Source<T>.value` and `IntBox.value`, so it is an `Int` for the arithmetic below.
interface Source<out T> {
    val value: T

    fun describe(): String =
        when (this) {
            is IntBox -> "int" + (value + 1)
            is ListBox -> "list" + value.size
            else -> "other"
        }
}

class IntBox(override val value: Int) : Source<Int>
class ListBox(override val value: List<String>) : Source<List<String>>
class TextBox(override val value: String) : Source<String>

fun box(): String {
    if (IntBox(4).describe() != "int5") return "int: " + IntBox(4).describe()
    if (ListBox(listOf("a", "b")).describe() != "list2") return "list"
    if (TextBox("x").describe() != "other") return "other"
    return "OK"
}
