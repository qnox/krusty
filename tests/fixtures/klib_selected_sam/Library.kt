package fixture

fun interface Action<T> {
    fun apply(value: T): T
}

private val initializerOnly = Action<String> { it + ":initialized" }

fun make(): Action<String> = Action<String> { it + ":direct" }

inline fun inlineMake(): Action<String> = Action<String> { it + ":inline" }

fun functionValue(value: (String) -> String): Action<String> = Action(value)

interface Root<T> { fun get(): T }
interface Leaf<T> : Root<T>
fun interface StoredAction<T> { fun apply(value: T): T }
private class HiddenWithSam : Leaf<String> {
    val converted = StoredAction<String> { it }
    override fun get(): String = "stored"
}
fun erasedObject(): Any = HiddenWithSam()
