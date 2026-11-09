// A member written with a definitely-non-null `T & Any` keeps that `& Any` when it is read
// through a projected receiver: through `Holder<*>` the value is `Any` and `Box<T & Any>` is
// `Box<out Any>`, through `Holder<out CharSequence?>` they are `CharSequence` and
// `Box<out CharSequence>`, and through `Holder<in String?>` the box is `Box<in String>`.

class Box<T>(val item: T)

abstract class Node<T>

abstract class Holder<T> : Node<T>() {
    abstract val value: T & Any
    abstract val box: Box<T & Any>
}

class Text(override val value: String) : Holder<String?>() {
    override val box: Box<String> = Box(value)
}

fun star(holder: Holder<*>): String {
    val value: Any = holder.value
    val box: Box<out Any> = holder.box
    return value.toString() + box.item
}

fun covariant(holder: Holder<out CharSequence?>): String {
    val value: CharSequence = holder.value
    val box: Box<out CharSequence> = holder.box
    return value.toString() + box.item.length
}

fun contravariant(holder: Holder<in String?>): Box<in String> = holder.box

// A safe cast without written type arguments takes them from the cast value: `Holder<*>`.
fun cast(node: Node<*>): Box<out Any>? = (node as? Holder)?.box

fun box(): String {
    val text = Text("O")
    if (star(text) != "OO") return "star: ${star(text)}"
    if (covariant(text) != "O1") return "covariant: ${covariant(text)}"
    if (contravariant(text).item != "O") return "contravariant"
    if (cast(text)?.item != "O") return "cast"
    return "OK"
}
