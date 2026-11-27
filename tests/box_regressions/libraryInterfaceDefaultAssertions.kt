// WITH_STDLIB
// kotlin.test's top-level assertions delegate to a library object (`DefaultAsserter`, and on Wasm
// `DefaultWasmAsserter`, which reaches its inherited bodies through a `super` call) whose
// `assertEquals` and `assertTrue` are default bodies the object inherits from the `Asserter`
// interface.
import kotlin.test.assertEquals
import kotlin.test.assertTrue

fun box(): String {
    assertEquals(1, 1)
    assertTrue(true)
    return "OK"
}
