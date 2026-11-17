// Library exception classes a target does not lay out itself: their constructors come from the
// library, and a catch of one tests against the class its unit lays out.
fun fail(reason: Any?): Nothing = throw AssertionError(reason)

fun box(): String {
    val message = try {
        fail("rejected")
    } catch (e: AssertionError) {
        e.message
    }
    if (message != "rejected") return "Fail: $message"
    val caught = try {
        throw Error("plain", IllegalStateException("cause"))
    } catch (e: Error) {
        e.cause?.message
    }
    if (caught != "cause") return "Fail cause: $caught"
    return "OK"
}
