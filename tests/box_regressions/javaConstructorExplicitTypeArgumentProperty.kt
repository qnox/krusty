// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: `java.util.concurrent.atomic.AtomicReference` is a Java class: its constructor
// parameter is the platform type `V!`. A property initialized from it with explicit type arguments
// and a generic argument whose own inference needs that parameter publishes the explicit type.
import java.util.concurrent.atomic.AtomicReference

class Slot<T>(val label: String)

fun <T> slot(): Slot<T> = Slot("K")

class Holder {
    private val state = AtomicReference<Pair<String, Slot<Unit>>>("O" to slot())
    val top = AtomicReference<Pair<String, Slot<Int>>>(Pair("O", slot()))

    fun read(): String {
        val (first, second) = state.get()
        return first + second.label
    }

    fun first(): String = state.get().first
}

val shared = AtomicReference<Pair<String, Slot<Unit>>>("O" to slot())

fun box(): String {
    val holder = Holder()
    if (holder.first() != "O") return "fail first"
    if (holder.top.get().second.label != "K") return "fail top"
    if (shared.get().first != "O") return "fail shared"
    return holder.read()
}
