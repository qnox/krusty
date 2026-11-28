// WITH_STDLIB
// MODULE: lib

// Function values of several operand counts, made by a module that declares more of them than the
// module calling them does: a call through a function type must find the body by what the type
// is, not by a table number the two modules would each have to count alike.
class Doubler : (Int) -> Int {
    override fun invoke(x: Int): Int = x * 2
}

class Joiner : (String, String) -> String {
    override fun invoke(a: String, b: String): String = a + b
}

open class Namer : () -> String {
    override fun invoke(): String = "lib"
}

class Widest : (String, String, String, String, String) -> String {
    override fun invoke(a: String, b: String, c: String, d: String, e: String): String = a + b + c + d + e
}

class Suspender : suspend (Int) -> String {
    override suspend fun invoke(x: Int): String = "suspend" + x
}

class TwoSuspender : suspend (Int, Int) -> String {
    override suspend fun invoke(x: Int, y: Int): String = "two" + x + y
}

fun threeOperands(): (Int, Int, Int) -> Int = { a, b, c -> a + b + c }

fun fourOperands(): (String, String, String, String) -> String = { a, b, c, d -> a + b + c + d }

fun suspendLambda(): suspend (String) -> String = { it + "!" }

// MODULE: main(lib)
import kotlin.coroutines.*

class Local : () -> String {
    override fun invoke(): String = "local"
}

class Renamed : Namer() {
    override fun invoke(): String = "renamed"
}

class Inherits : Namer()

class LocalSuspender : suspend (Int) -> String {
    override suspend fun invoke(x: Int): String = "local" + x
}

suspend fun callSuspend(function: suspend (Int) -> String, argument: Int): String = function(argument)

suspend fun callTwo(function: suspend (Int, Int) -> String): String = function(3, 4)

suspend fun runMain(): String {
    if (callSuspend(Suspender(), 1) != "suspend1") return "library suspend class"
    if (callSuspend(LocalSuspender(), 2) != "local2") return "local suspend class"
    if (callSuspend({ "lambda" + it }, 3) != "lambda3") return "local suspend lambda"
    if (callTwo(TwoSuspender()) != "two34") return "library two operand suspend class"
    if (suspendLambda()("a") != "a!") return "library suspend lambda"
    return "OK"
}

fun box(): String {
    val double: (Int) -> Int = Doubler()
    if (double(4) != 8) return "library class over an Int"
    val join: (String, String) -> String = Joiner()
    if (join("a", "b") != "ab") return "library class over references"
    val namers: List<() -> String> = listOf(Namer(), Renamed(), Inherits(), Local(), { "lambda" })
    if (namers.map { it() } != listOf("lib", "renamed", "lib", "local", "lambda")) return "zero operands"
    val widest: (String, String, String, String, String) -> String = Widest()
    if (widest("a", "b", "c", "d", "e") != "abcde") return "library class of five operands"
    if (threeOperands()(1, 2, 3) != 6) return "library lambda of three operands"
    if (fourOperands()("a", "b", "c", "d") != "abcd") return "library lambda of four operands"
    var result = "FAIL: not run"
    val body: suspend () -> Unit = { result = runMain() }
    body.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
    return result
}
