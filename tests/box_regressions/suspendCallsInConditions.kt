// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

suspend fun positive(n: Int): Boolean = n > 0

suspend fun label(n: Int): String = if (positive(n)) "pos" else "neg"

suspend fun both(a: Int, b: Int): String =
    if (positive(a) && positive(b)) "both" else if (positive(a) || positive(b)) "one" else "none"

suspend fun inWhen(n: Int): String = when {
    !positive(n) -> "neg"
    positive(n - 10) -> "big"
    else -> "small"
}

suspend fun inStatement(n: Int): Int {
    var count = 0
    if (positive(n)) count += 1
    if (positive(-n)) count += 10
    return count
}

interface Checker {
    suspend fun check(n: Int): Boolean
}

// An open member is reached through a bridge, so its decision tree is built after checking.
abstract class Describer : Checker {
    open suspend fun describe(n: Int): String = if (check(n)) "yes" else "no"
}

class Positive : Describer() {
    override suspend fun check(n: Int): Boolean = n > 0
}

class Delegating(private val inner: Checker) : Checker by inner {
    suspend fun twice(n: Int): String = if (check(n) && check(n + 1)) "twice" else if (check(n)) "once" else "never"
}

suspend fun elvis(n: Int?): String = if (n == null || positive(n)) "ok" else "bad"

suspend fun compare(a: Int, b: Int): String = if (positive(a) == positive(b)) "same" else "differ"

suspend fun argument(n: Int): String = label(if (positive(n)) n else -n) + (if (positive(n)) "+" else "-")

suspend fun loop(n: Int): Int {
    var i = n
    var steps = 0
    while (positive(i)) { i--; steps++ }
    return steps
}

suspend fun negated(n: Int): String = if (!(positive(n) && !positive(n - 5))) "out" else "in"

fun run(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    var result = "not run"
    run {
        val parts = listOf(
            label(1), label(-1), both(1, 1), both(1, -1), both(-1, -1),
            inWhen(-5), inWhen(50), inWhen(5), inStatement(3).toString(),
            Delegating(Positive()).twice(1), Delegating(Positive()).twice(-1), Positive().describe(1), elvis(null), elvis(-1),
            compare(1, 2), compare(1, -2), argument(3), argument(-3), loop(4).toString(), negated(2), negated(9),
        )
        result = parts.joinToString(",")
    }
    return if (result == "pos,neg,both,one,none,neg,big,small,1,twice,never,yes,ok,bad,same,differ,pos+,pos-,4,in,out") "OK" else result
}
