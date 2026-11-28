// A library class whose primary constructor declares a default parameter and delegates to a
// secondary constructor of its superclass, constructed with every argument supplied.
fun box(): String {
    try {
        throw NotImplementedError("pending")
    } catch (e: NotImplementedError) {
        if (e.message != "pending") return "Fail message: ${e.message}"
    }
    try {
        throw NotImplementedError("pending")
    } catch (e: Error) {
        return "OK"
    }
}
