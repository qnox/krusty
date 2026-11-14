// WITH_STDLIB
// FILE: HashFailureDeclaration.kt
fun makeHashFailure(): IllegalStateException = IllegalStateException("message")

// FILE: HashFailureUse.kt
fun box(): String {
    val failure = makeHashFailure()
    val value: Any = failure
    val before = value.hashCode()
    try {
        throw failure
    } catch (caught: Throwable) {
        if (caught !== failure || caught.message != "message") return "throwable reference or message"
        if (caught.hashCode() != before || value.hashCode() != before) return "throwable hash"
    }
    val another: Any = IllegalArgumentException("other")
    if (another === value || another.hashCode() != another.hashCode()) return "another throwable"
    return "OK"
}
