// LANGUAGE: +ContextParameters
// MODULE: lib
// FILE: Slot.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class Id(val raw: Int)

class Slot<T>(var stored: T) {
    context(unit: Int)
    var value: T
        get() = stored
        set(next) { stored = next }
}

// MODULE: box(lib)
// FILE: box.kt
fun box(): String {
    val slot = Slot(Id(1))
    with(2) {
        slot.value = Id(5)
        if (slot.stored != Id(5)) return "setter: ${slot.stored}"
        if (slot.value != Id(5)) return "getter: ${slot.value}"
    }
    return "OK"
}
