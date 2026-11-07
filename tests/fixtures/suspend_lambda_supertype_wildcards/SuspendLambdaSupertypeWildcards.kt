// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

interface Item
class Box(val name: String) : Item

fun <T> capture(block: suspend String.() -> T): suspend String.() -> T = block

fun <T> run(receiver: String, block: suspend String.() -> T): T? {
    var result: T? = null
    val store: suspend String.() -> Unit = { result = block() }
    store.startCoroutine(receiver, Continuation(EmptyCoroutineContext) {})
    return result
}

// A suspend lambda's `FunctionN` supertype spells declaration-site variance below its arguments:
// `Continuation<-List<+Box>>`, even over a final class, while `invoke` keeps the parameter rules.
fun box(): String {
    val items = capture { listOf<Item>(Box(this)) }
    val boxes = capture { listOf(Box(this)) }
    val nested: suspend (List<Box>) -> Map<String, List<Item>> = { mapOf("k" to it) }
    val anything: suspend () -> Any? = { "unused" }
    val first = (run("O", items)?.single() as Box).name
    val second = run("K", boxes)?.single()?.name
    return if (nested.hashCode() != 0 && anything.hashCode() != 0) first + second else "fail"
}
