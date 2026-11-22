// WITH_STDLIB
import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

// A function type's members are its function classifier's: a bare `invoke(...)` with a
// function-typed implicit receiver calls `FunctionN.invoke`, through a typealias too.
typealias ElementFilter = (List<String>) -> List<String>

fun ElementFilter.matches(value: String): Boolean = invoke(listOf(value)).isNotEmpty()

fun ((Int) -> Int).twice(value: Int): Int = invoke(invoke(value))

fun (Int.(Int) -> Int).applyTo(value: Int): Int = invoke(value, 2)

suspend fun (suspend (Int) -> Int).callSuspend(value: Int): Int = invoke(value)

fun runSuspend(block: suspend () -> Int): Int {
    var result = 0
    block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    return result
}

fun suspendShape(block: suspend () -> Int): Int = with(block) { if (hashCode() == hashCode()) 1 else 0 }

fun callThrough(function: (Int) -> Int): Int = with(function) { invoke(10) }

// `FunctionN<P..., R>?` is the nullable function type `((P...) -> R)?`.
class CallOnce<F, T>(private val defaultValue: T, delegate: Function1<F, T>) : Function1<F, T> {
    private var function: Function1<F, T>? = delegate

    private fun take(): Function1<F, T>? {
        val current = function
        function = null
        return current
    }

    override fun invoke(p1: F): T = take()?.invoke(p1) ?: defaultValue
}

fun nominal(function: Function1<Int, Int>): Function1<Int, Int>? = function

fun fromLambda(function: (Int) -> Int): Function1<Int, Int>? = function

fun box(): String {
    val keep: ElementFilter = { it }
    val drop: ElementFilter = { emptyList() }
    if (!keep.matches("a") || drop.matches("a")) return "fail: typealias receiver"
    if ({ value: Int -> value + 1 }.twice(1) != 3) return "fail: function receiver"
    val scale: Int.(Int) -> Int = { factor -> this * factor }
    if (scale.applyTo(21) != 42) return "fail: extension function receiver"
    if (suspendShape { 1 } != 1) return "fail: suspend function receiver"
    val inc: suspend (Int) -> Int = { it + 1 }
    if (runSuspend { inc.callSuspend(41) } != 42) return "fail: suspend invoke"
    if (callThrough { it * 3 } != 30) return "fail: with receiver"
    val double: (Int) -> Int = { it * 2 }
    val once = CallOnce(-1, double)
    if (once(4) != 8 || once(4) != -1) return "fail: call once"
    val triple: Function1<Int, Int> = { it * 3 }
    val explicit = CallOnce<Int, Int>(-1, triple)
    if (explicit(2) != 6 || explicit(2) != -1) return "fail: explicit call once"
    if (nominal { it + 1 }?.invoke(5) != 6) return "fail: nominal nullable"
    if (fromLambda { it - 1 }?.invoke(6) != 5) return "fail: lambda nullable"
    return "OK"
}
