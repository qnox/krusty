fun <R> inner(block: () -> R): R = block()

fun <T, R> outer(value: T, transform: (T) -> R): R = transform(value)

fun relay() = outer(41) { inner { it + 1 } }

fun box(): String = if (relay() == 42) "OK" else "FAIL"
