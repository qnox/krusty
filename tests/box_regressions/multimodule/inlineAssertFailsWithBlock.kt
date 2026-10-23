// WITH_STDLIB
// MODULE: lib
import kotlin.test.*
fun inlineBehavior(): String {
    val thrown = assertFailsWith<IllegalStateException> { error("boom") }
    if (thrown.message != "boom") return "fail: ${thrown.message}"
    val any = assertFails { throw IllegalArgumentException("any") }
    return if (any.message == "any") "OK" else "fail: ${any.message}"
}
// MODULE: main(lib)
fun box(): String = inlineBehavior()
