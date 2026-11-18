package fixture

fun interface Action<T> {
    fun apply(value: T): T
}

private val initializerOnly = Action<String> { it }

fun make(): Action<String> = Action<String> { value -> value }
