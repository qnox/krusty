// WITH_STDLIB
// MODULE: lib
import kotlin.test.*
fun inlineBehavior(): String = try {
    assertFailsWith<IllegalStateException> { }
    "fail: no assertion error"
} catch (e: AssertionError) {
    "OK"
}
// MODULE: main(lib)
fun box(): String = inlineBehavior()
