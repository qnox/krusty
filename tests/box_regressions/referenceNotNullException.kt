// MODULE: lib
var assertionInputs = 0

fun assertionInput(value: String?): String? {
    assertionInputs++
    return value
}

fun assertReference(value: String?): String = assertionInput(value)!!

fun <T> hiddenNullableAssertion(): T {
    val value: T? = null
    return value!!
}

fun assertionInputCount(): Int = assertionInputs

// MODULE: main(lib)
fun box(): String {
    if (assertReference("OK") != "OK") return "nonnull value"
    try {
        assertReference(null)
        return "missing reference exception"
    } catch (error: Throwable) {
        if (error !is NullPointerException) return "wrong reference exception"
        if (error.message != null) return "reference exception message"
    }
    try {
        hiddenNullableAssertion<String>()
        return "missing generic exception"
    } catch (error: Throwable) {
        if (error !is NullPointerException) return "wrong generic exception"
        if (error.message != null) return "generic exception message"
    }
    if (assertionInputCount() != 2) return "operand evaluation"
    return "OK"
}
