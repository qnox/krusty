var equalsCalls = 0

class EqualToNull {
    override fun equals(other: Any?): Boolean {
        equalsCalls += 1
        return other === null
    }

    override fun hashCode(): Int = 0
}

fun keep(value: Any?): Any = value ?: "missing"

fun classify(value: Any?): String {
    if (value !is Throwable) return "not throwable"
    if (value is Exception) return "unexpected exception"
    if (value is RuntimeException) return "unexpected runtime exception"
    if (value is IndexOutOfBoundsException) return "unexpected bounds exception"
    return value.message ?: "missing message"
}

fun box(): String {
    val ordinary = EqualToNull()
    if (keep(ordinary) !== ordinary) return "elvis used equals"
    if (equalsCalls != 0) return "null guard called equals"
    if (keep(null) != "missing") return "null fallback"
    if (classify(null) != "not throwable") return "null type test"
    if (classify("text") != "not throwable") return "string type test"
    val failure = Throwable("OK")
    if (classify(failure) != "OK") return "hierarchy type test"
    try {
        throw failure
    } catch (same: Throwable) {
        if (same !== failure) return "changed identity"
        return classify(same)
    }
}
