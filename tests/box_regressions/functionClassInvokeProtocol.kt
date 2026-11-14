// WITH_STDLIB

var calls = 0
open class RenderNumber : (Int) -> String {
    override fun invoke(value: Int): String {
        calls++
        return "number:$value"
    }
}
class InheritedRenderNumber : RenderNumber()
class Answer : () -> Int {
    override fun invoke(): Int = 42
}
fun eraseCallable(value: Any): Any = value
fun renderNumber(value: Int): String = "number:$value"

fun box(): String {
    val inherited: (Int) -> String = InheritedRenderNumber()
    if (inherited(7) != "number:7" || calls != 1) return "ordinary invoke"
    val erased = eraseCallable(inherited)
    val cast = erased as (Int) -> String
    if (cast(8) != "number:8" || calls != 2) return "cast invoke"
    val safe = erased as? (Int) -> String ?: return "safe cast"
    if (safe(9) != "number:9" || calls != 3) return "safe invoke"
    if (eraseCallable(Any()) as? (Int) -> String != null) return "non-callable"
    val absent: Any? = null
    if (absent as? (Int) -> String != null) return "null callable"
    val zero: () -> Int = Answer()
    if (zero() != 42) return "zero arity"
    val lambda: (Int) -> String = { "number:$it" }
    val reference: (Int) -> String = ::renderNumber
    if (lambda(10) != "number:10" || reference(11) != "number:11") return "generated entries"
    return "OK"
}
