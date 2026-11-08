var selectedHandlers = 0

fun orderedCatch(value: Throwable): String = try {
    throw value
} catch (caught: IllegalStateException) {
    selectedHandlers += 1
    if (caught !== value) "changed identity" else caught.message ?: "missing message"
} catch (caught: IllegalArgumentException) {
    selectedHandlers += 10
    if (caught !== value) "changed sibling identity" else caught.message ?: "missing message"
} catch (caught: RuntimeException) {
    selectedHandlers += 100
    if (caught !== value) "changed parent identity" else "parent"
} catch (caught: Throwable) {
    selectedHandlers += 1000
    if (caught !== value) "changed base identity" else "base"
}

fun unmatchedCatch(value: Throwable): Throwable = try {
    try {
        throw value
    } catch (wrong: IllegalArgumentException) {
        throw IllegalStateException("wrong catch")
    }
} catch (caught: Throwable) {
    caught
}

fun handlerThrow(value: Throwable): Throwable = try {
    try {
        throw IllegalStateException("first")
    } catch (caught: IllegalStateException) {
        throw value
    } catch (wrong: IllegalArgumentException) {
        throw IllegalStateException("sibling caught handler")
    }
} catch (caught: Throwable) {
    caught
}

fun normalCompletion(): Int = try {
    42
} catch (wrong: IllegalStateException) {
    -1
} catch (wrong: Throwable) {
    -2
}

fun box(): String {
    val leaf = IllegalStateException("OK")
    if (orderedCatch(leaf) != "OK" || selectedHandlers != 1) return "leaf order"
    val sibling = IllegalArgumentException("argument")
    if (orderedCatch(sibling) != "argument" || selectedHandlers != 11) return "sibling order"
    val parent = RuntimeException("parent")
    if (orderedCatch(parent) != "parent" || selectedHandlers != 111) return "parent order"
    val base = Throwable("base")
    if (orderedCatch(base) != "base" || selectedHandlers != 1111) return "base order"
    if (unmatchedCatch(leaf) !== leaf) return "unmatched identity"
    val fromHandler = IllegalArgumentException("second")
    if (handlerThrow(fromHandler) !== fromHandler) return "handler escape"
    if (normalCompletion() != 42) return "normal completion"
    return "OK"
}
