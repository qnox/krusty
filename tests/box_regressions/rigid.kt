// LANGUAGE: +ContextParameters
class Slot<T>(val value: T)

fun <X> pick(outer: Slot<X>): X {
    context(c: C)
    fun <C : Slot<X>> read(): X = c.value
    return context(outer) {
        context(Slot(1)) {
            read()
        }
    }
}

fun box(): String = pick(Slot("OK"))
