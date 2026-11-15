// A concrete override of a generic member keeps working through the base whose accessors carry
// the value as a reference: a `String` getter and setter stand in `T & Any` and `T` slots.

abstract class Holder<T> {
    abstract val value: T & Any
    abstract var slot: T
}

open class Text(override val value: String) : Holder<String?>() {
    override var slot: String? = null
}

class Shout(value: String) : Text(value) {
    override val value: String = value + "!"
}

abstract class Counter<T> {
    abstract val value: T & Any
}

class Count(override val value: String) : Counter<String>()

fun read(holder: Holder<String?>): String = holder.value

fun write(holder: Holder<String?>, value: String?) {
    holder.slot = value
}

fun box(): String {
    val text = Text("O")
    if (read(text) != "O") return "base read: ${read(text)}"
    write(text, "K")
    if (text.slot != "K") return "base write: ${text.slot}"
    val shout: Holder<String?> = Shout("O")
    if (shout.value != "O!") return "subclass: ${shout.value}"
    val counter: Counter<String> = Count("K")
    return text.value + counter.value
}
