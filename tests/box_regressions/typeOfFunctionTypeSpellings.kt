// A function type and its `FunctionN` spelling name one classifier, so `typeOf` gives one type for
// both: equal, hashing alike and printing alike. Swapping the arguments gives another type.
import kotlin.reflect.typeOf

class A
class B

fun box(): String {
    val sugar = typeOf<(A) -> B>()
    val named = typeOf<Function1<A, B>>()
    if (sugar != named) return "equal: $sugar $named"
    if (sugar.hashCode() != named.hashCode()) return "hash"
    if (sugar.toString() != named.toString()) return "text: $sugar $named"
    if (typeOf<(A) -> B>() == typeOf<(B) -> A>()) return "argument order"
    if (typeOf<() -> Unit>() != typeOf<Function0<Unit>>()) return "arity 0"
    return "OK"
}
