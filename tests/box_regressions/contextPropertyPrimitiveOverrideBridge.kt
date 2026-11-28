// LANGUAGE: +ContextParameters
abstract class Slots<T> {
    context(unit: Int)
    abstract var slot: T

    context(unit: Int)
    fun write(value: T) { slot = value }

    context(unit: Int)
    fun read(): T = slot
}

class IntSlots : Slots<Int>() {
    var stored = 0

    context(unit: Int)
    override var slot: Int
        get() = stored + unit
        set(value) { stored = value * 10 }
}

fun box(): String {
    val slots: Slots<Int> = IntSlots()
    with(2) {
        slots.write(4)
        if (slots.read() != 42) return "through the supertype: ${slots.read()}"
    }
    return "OK"
}
