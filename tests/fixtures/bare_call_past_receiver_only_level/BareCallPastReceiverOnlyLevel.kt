// A same-package extension named `check` needs a receiver, so a bare `check(...)` passes over it
// to the default-imported `kotlin.check`, whose contract still smart-casts its argument.
class Logger {
    var errors = 0
}

internal inline fun Logger.check(condition: Boolean, message: () -> String) {
    if (!condition) errors += message().length
}

fun guard(value: Any): String {
    check(value is String)
    return value
}

fun box(): String {
    val logger = Logger()
    logger.check(false) { "x" }
    if (logger.errors != 1) return "logger"
    return guard("OK")
}
