// WITH_STDLIB

var calls = 0
open class RenderNumber : (Int) -> String {
    override fun invoke(value: Int): String {
        calls++
        return "number:$value"
    }
}
class InheritedRenderNumber : RenderNumber()
class Prefix : (String) -> String {
    override fun invoke(value: String): String = "prefix:$value"
}
var order = ""
fun choose(value: (Int) -> String): (Int) -> String {
    order += "receiver;"
    return value
}
fun argument(): Int {
    order += "argument;"
    return 12
}
fun <T, R> applyCallable(function: (T) -> R, argument: T): R = function(argument)
class Answer : () -> Int {
    override fun invoke(): Int = 42
}
fun eraseCallable(value: Any): Any = value
fun renderNumber(value: Int): String = "number:$value"

fun box(): String {
    val inherited: (Int) -> String = InheritedRenderNumber()
    if (inherited(7) != "number:7" || calls != 1) return "ordinary invoke"
    val erased = eraseCallable(inherited)
    val cast = erased as ((Int) -> String)
    if (cast !== inherited) return "cast identity"
    if (cast(8) != "number:8" || calls != 2) return "cast invoke"
    val safe = (erased as? ((Int) -> String)) ?: return "safe cast"
    if (safe(9) != "number:9" || calls != 3) return "safe invoke"
    if ((eraseCallable(Any()) as? ((Int) -> String)) != null) return "non-callable"
    val absent: Any? = null
    if ((absent as? ((Int) -> String)) != null) return "null callable"
    val zero: () -> Int = Answer()
    if (zero() != 42) return "zero arity"
    val lambda: (Int) -> String = { "number:$it" }
    val reference: (Int) -> String = ::renderNumber
    if (lambda(10) != "number:10" || reference(11) != "number:11") return "generated entries"
    if (erased !is Function1<*, *>) return "ordinary membership"
    if (erased is Function0<*> || erased is Function2<*, *, *>) return "arity membership"
    if (eraseCallable(lambda) !is Function1<*, *> || eraseCallable(reference) !is Function1<*, *>) return "generated membership"
    if ((erased as? (() -> String)) != null) return "wrong arity"
    val prefix: (String) -> String = Prefix()
    if (applyCallable(prefix, "OK") != "prefix:OK") return "reference argument"
    if (applyCallable(reference, 13) != "number:13") return "generic callable"
    if (choose(inherited)(argument()) != "number:12" || calls != 4) return "evaluated invocation"
    if (order != "receiver;argument;") return "evaluation order"
    return "OK"
}
