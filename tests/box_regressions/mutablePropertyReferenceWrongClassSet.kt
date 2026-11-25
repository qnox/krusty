// A mutable property reference called through its erased `set` checks the written value against the
// property's declared type before it writes: a value of another class fails with a
// `ClassCastException` and leaves the property as it was.
import kotlin.reflect.KMutableProperty0

class A
class B

var slot: A = A()

@Suppress("UNCHECKED_CAST")
fun box(): String {
    val original = slot
    val erased = ::slot as KMutableProperty0<Any?>
    try {
        erased.set(B())
        return "FAIL accepted"
    } catch (e: ClassCastException) {
    }
    if (slot !== original) return "FAIL mutated"
    return "OK"
}
