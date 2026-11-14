// WITH_STDLIB

class NamedFailure : IllegalStateException("marker") {
    override fun toString(): String = "named failure"
}

fun renderFailure(failure: Throwable): String = failure.toString()

fun box(): String {
    val callback: () -> Int = { 42 }
    val floating: Any = 1.25
    if (callback() != 42 || floating !== floating) return "unrelated values"
    if (renderFailure(NamedFailure()) != "named failure") return "override"
    val original = IllegalStateException("marker")
    if (!renderFailure(original).endsWith(": marker")) return "runtime message"
    try {
        throw NamedFailure()
    } catch (failure: Throwable) {
        if (renderFailure(failure) != "named failure") return "caught override"
    }
    return "OK"
}
