// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

class Team(val name: String)

suspend fun team(name: String?): Team? = name?.let(::Team)

suspend fun label(): String = "O"

suspend fun count(): Int? = 2

fun <T> keep(value: T): T = value

// A suspension point's erased result reaches each consumer as the `Object` it returned; only a
// consumer that needs a narrower type casts it.
suspend fun isTeam(): Boolean = team("a") is Team

suspend fun missing(): Boolean = team(null) == null

suspend fun asTeam(): Team = team("b") as Team

suspend fun kept(): Any? = keep(label())

suspend fun stored(): Any {
    val value: Any = label()
    return value
}

suspend fun assigned(): Any {
    var value: Any = ""
    value = label()
    return value
}

suspend fun incremented(): Int = count()!! + 1

fun run(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    var result = "not run"
    run {
        result = if (isTeam() && missing() && asTeam().name == "b" && kept() == "O" &&
            stored() == "O" && incremented() == 3
        ) {
            assigned().toString() + "K"
        } else {
            "fail"
        }
    }
    return result
}
