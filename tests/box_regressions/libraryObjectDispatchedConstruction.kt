// WITH_STDLIB
// The source reads a library object and calls its member through the interface it implements.
// Only dispatch reaches the object's `fail`, whose body constructs the library's AssertionError.
import kotlin.test.Asserter
import kotlin.test.DefaultAsserter

fun box(): String {
    val asserter: Asserter = DefaultAsserter
    val message = try {
        asserter.fail("rejected")
    } catch (e: AssertionError) {
        e.message
    }
    if (message != "rejected") return "Fail: $message"
    return "OK"
}
