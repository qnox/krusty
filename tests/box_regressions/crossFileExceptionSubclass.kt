// FILE: base.kt
open class AppFailure(message: String?) : IllegalStateException(message) {
    open fun code(): Int = 1
}

open class CausedFailure(message: String, cause: Throwable) : RuntimeException(message, cause)

// FILE: main.kt
class MissingFailure(val what: String) : AppFailure("missing " + what) {
    override fun code(): Int = super.code() + 1
}

class WrappedFailure : CausedFailure {
    constructor(cause: Throwable) : super("wrapped", cause)
}

fun fail(what: String): Nothing = throw MissingFailure(what)

fun box(): String {
    try {
        fail("key")
    } catch (e: MissingFailure) {
        if (e.message != "missing key") return "fail message: ${e.message}"
        if (e.code() != 2) return "fail code: ${e.code()}"
        if (e.what != "key") return "fail what"
        // In the root package, so every target names the class the same way.
        if (e.toString() != "MissingFailure: missing key") return "fail toString: $e"
    }
    try {
        fail("value")
    } catch (e: IllegalStateException) {
        if (e.message != "missing value") return "fail state message: ${e.message}"
    }
    val inner = IllegalArgumentException("inner")
    try {
        throw WrappedFailure(inner)
    } catch (e: RuntimeException) {
        if (e.message != "wrapped") return "fail wrapped message: ${e.message}"
        if (e.cause !== inner) return "fail wrapped cause: ${e.cause}"
    }
    return "OK"
}
