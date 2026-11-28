// LANGUAGE: +ContextParameters
// MODULE: lib
// FILE: Slots.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class Id(val raw: Int)

abstract class Slots {
    context(unit: Int)
    abstract var slot: Id

    context(unit: Int)
    fun readSlot(): Id = slot

    context(unit: Int)
    fun writeSlot(value: Id) {
        slot = value
    }
}

// MODULE: box(lib)
// FILE: box.kt
class IdSlots : Slots() {
    var stored = Id(0)

    context(unit: Int)
    override var slot: Id
        get() = Id(stored.raw * unit)
        set(value) {
            stored = Id(value.raw + unit)
        }
}

fun box(): String {
    val slots = IdSlots()
    with(2) {
        slots.slot = Id(5)
        if (slots.stored != Id(7)) return "setter: ${slots.stored}"
        if (slots.slot != Id(14)) return "getter: ${slots.slot}"
        slots.writeSlot(Id(1))
        if (slots.stored != Id(3)) return "writer: ${slots.stored}"
        if (slots.readSlot() != Id(6)) return "reader: ${slots.readSlot()}"
    }
    return "OK"
}
