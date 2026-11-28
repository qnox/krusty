// LANGUAGE: +ContextParameters
abstract class Slots<T> {
    context(unit: Int)
    abstract var slot: T
    context(unit: Int)
    fun write(value: T) { slot = value }
    context(unit: Int)
    fun read(): T = slot
}
class TextSlots : Slots<String>() {
    var stored = ""
    context(unit: Int)
    override var slot: String
        get() = stored + unit
        set(value) { stored = value + "-" }
}
fun box(): String {
    val slots: Slots<String> = TextSlots()
    with(2) {
        slots.write("v")
        if (slots.read() != "v-2") return "through the supertype: ${slots.read()}"
    }
    return "OK"
}
