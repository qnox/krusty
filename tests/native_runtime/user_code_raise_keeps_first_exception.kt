// Kotlin's answers for `user_code_raise_keeps_first_exception.c`: a runtime entry that calls into
// the program stops where the program raised, and the program's exception is the one that arrives.
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotSame
import kotlin.test.assertSame

class Boom : Exception("boom")

class Raising {
    override fun equals(other: Any?): Boolean = throw Boom()
}

class Unprintable {
    fun f() = 0
    override fun equals(other: Any?): Boolean = throw Boom()
    override fun hashCode(): Int = throw Boom()
    override fun toString(): String = throw Boom()
}

class UnprintableException : Exception() {
    override fun toString(): String = throw Boom()
}

fun box(): String {
    val out = StringBuilder()
    fun outcome(label: String, block: () -> Any?) {
        val answer = try {
            block()
            "completed"
        } catch (e: Throwable) {
            "threw ${e::class.simpleName}"
        }
        out.append("$label = $answer\n")
    }
    val one: Any = 1
    val u = Unprintable()
    outcome("assertEquals(Raising(), 1)") { assertEquals<Any>(Raising(), one) }
    outcome("assertEquals(1, Raising())") { assertEquals<Any>(one, Raising()) }
    outcome("assertEquals(u, 1)") { assertEquals<Any>(u, one) }
    outcome("assertEquals(1, u, m)") { assertEquals<Any>(one, u, "m") }
    outcome("assertSame(u, 1)") { assertSame<Any>(u, one) }
    outcome("assertSame(1, u)") { assertSame<Any>(one, u) }
    outcome("assertNotSame(u, u)") { assertNotSame<Any>(u, u) }
    outcome("assertFailsWith, UnprintableException thrown") {
        assertFailsWith<IllegalArgumentException> { throw UnprintableException() }
    }
    outcome("RuntimeException(UnprintableException())") {
        RuntimeException(UnprintableException())
    }
    outcome("u::f == Unprintable()::f") { (u::f as Any) == (Unprintable()::f as Any) }
    outcome("(u::f).hashCode()") { (u::f as Any).hashCode() }
    return out.toString()
}
