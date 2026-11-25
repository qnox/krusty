// Writing `null` through a reference to a property of a type parameter whose bound admits null is
// not a cast failure: the check the erased write makes keeps the bound's nullability.
import kotlin.reflect.KMutableProperty0

class Holder<T : CharSequence?>(var value: T)

fun main0(): String? {
    val holder = Holder<String?>("x")
    val reference: KMutableProperty0<String?> = holder::value
    reference.set(null)
    return holder.value
}

fun box(): String {
    if (main0() != null) return "FAIL null write"
    return "OK"
}
