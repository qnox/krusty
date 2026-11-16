package fixture

fun interface Action<T> {
    fun apply(value: T): T
}

private val initializerOnly = Action<String> { it + ":initialized" }

fun make(): Action<String> = Action<String> { it + ":direct" }

inline fun inlineMake(): Action<String> = Action<String> { it + ":inline" }
