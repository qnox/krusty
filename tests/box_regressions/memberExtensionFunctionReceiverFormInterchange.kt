// A `String.() -> R` value stands where `(String) -> R` is expected and the reverse, so the
// function results still constrain a member extension's formal from either receiver form.

class Duo<out A, out B>(val first: A, val second: B)

open class Shape
class Circle : Shape()
class Square : Shape()

object Extensions {
    fun <T> Duo<String.() -> T, String.() -> T>.firstFromReceiver(): T = first("input")

    fun inferredReceiver(pair: Duo<(String) -> Circle, (String) -> Square>): Any =
        pair.firstFromReceiver()

    fun explicitReceiver(pair: Duo<(String) -> Circle, (String) -> Square>): Any =
        pair.firstFromReceiver<Any>()

    fun <T> Duo<(String) -> T, (String) -> T>.firstFromParameter(): T = first("input")

    fun inferredParameter(pair: Duo<String.() -> Circle, String.() -> Square>): Any =
        pair.firstFromParameter()

    fun explicitParameter(pair: Duo<String.() -> Circle, String.() -> Square>): Any =
        pair.firstFromParameter<Any>()
}

fun box(): String {
    val circle = Circle()
    val ordinaryFirst: (String) -> Circle = { circle }
    val ordinarySecond: (String) -> Square = { Square() }
    val ordinaryValues = Duo(ordinaryFirst, ordinarySecond)
    if (Extensions.inferredReceiver(ordinaryValues) !== circle) return "inferred receiver"
    if (Extensions.explicitReceiver(ordinaryValues) !== circle) return "explicit receiver"

    val receiverFirst: String.() -> Circle = { circle }
    val receiverSecond: String.() -> Square = { Square() }
    val receiverValues = Duo(receiverFirst, receiverSecond)
    if (Extensions.inferredParameter(receiverValues) !== circle) return "inferred parameter"
    if (Extensions.explicitParameter(receiverValues) !== circle) return "explicit parameter"
    return "OK"
}
