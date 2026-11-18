package fixture

// An unrelated interface whose abstract method has the same name and shape as `Action.apply`.
fun interface Other<T> {
    fun apply(value: T): T
}

fun other(): Other<String> = Other<String> { value -> value }
