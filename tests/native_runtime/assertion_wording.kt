// Kotlin's answers for `assertion_wording.c`: every kotlin.test failure text.
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotSame
import kotlin.test.assertSame
import kotlin.test.assertTrue

fun box(): String {
    val out = StringBuilder()
    fun failure(label: String, block: () -> Unit) {
        val answer = try {
            block()
            "passed"
        } catch (e: Throwable) {
            "${e::class.simpleName}: ${e.message}"
        }
        out.append("$label = $answer\n")
    }
    val x = listOf(1)
    failure("assertTrue(false)") { assertTrue(false) }
    failure("assertTrue(false, m)") { assertTrue(false, "m") }
    failure("assertFalse(true)") { assertFalse(true) }
    failure("assertFalse(true, m)") { assertFalse(true, "m") }
    failure("assertEquals(1, 2, m)") { assertEquals(1, 2, "m") }
    failure("assertSame([1], [1])") { assertSame(listOf(1), listOf(1)) }
    failure("assertSame([1], [1], m)") { assertSame(listOf(1), listOf(1), "m") }
    failure("assertNotSame(x, x)") { assertNotSame(x, x) }
    failure("assertNotSame(x, x, m)") { assertNotSame(x, x, "m") }
    failure("assertEquals(null, 2)") { assertEquals<Int?>(null, 2) }
    failure("assertTrue(true, m)") { assertTrue(true, "m") }
    failure("assertFalse(false, m)") { assertFalse(false, "m") }
    failure("assertNotSame([1], [1], m)") { assertNotSame(listOf(1), listOf(1), "m") }
    return out.toString()
}
