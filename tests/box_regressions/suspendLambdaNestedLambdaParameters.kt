// WITH_STDLIB

import kotlin.coroutines.Continuation
import kotlin.coroutines.EmptyCoroutineContext
import kotlin.coroutines.startCoroutine

class Scope(val base: Int)

fun <T> withScope(block: Scope.() -> T): T = Scope(40).block()

fun <T> mapped(value: String, transform: (String) -> T): T = transform(value)

fun run(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    var result = ""
    val suffix = "K"
    run {
        // Both lambdas below are private methods of the suspend lambda's class, so neither guards
        // its receiver or value parameter.
        val total = withScope { base + suffix.length + 1 }
        val text = mapped("O") { it + suffix }
        result = if (total == 42) text else "total $total"
    }
    // Outside a suspend lambda the same lambdas guard their parameters.
    val outside = withScope { base + 2 }
    return if (outside == 42) result else "outside $outside"
}
