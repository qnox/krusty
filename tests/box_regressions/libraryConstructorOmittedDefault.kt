// A library constructor's default parameter left out at the construction, and an inline library
// function (`TODO`) whose body does so.
fun box(): String {
    try {
        throw NotImplementedError()
    } catch (e: NotImplementedError) {
        if (e.message != "An operation is not implemented.") return "Fail message: ${e.message}"
    }
    try {
        TODO()
    } catch (e: NotImplementedError) {
        return "OK"
    }
    return "Fail: expected NotImplementedError"
}
