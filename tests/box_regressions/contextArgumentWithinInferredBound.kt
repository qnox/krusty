// LANGUAGE: +ContextParameters
// The return type is inferred from the context value's bound. A nearer value outside the bound
// is not a candidate, in signature evaluation as in the body check.
class Slot<T>(val value: T)

context(c: C)
fun <T, C : Slot<T>> value(): T = c.value

fun box(): String {
    val result: String = context(Slot("OK")) {
        context(42) {
            value()
        }
    }
    return result
}
