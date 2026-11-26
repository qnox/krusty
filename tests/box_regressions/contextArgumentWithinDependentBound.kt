// LANGUAGE: +ContextParameters
class Slot<T>(val value: T)

context(c: C)
fun <T, C : Slot<T>> read(x: T): T = c.value

fun box(): String = context(Slot("OK")) {
    read("")
}
