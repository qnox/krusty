// A member written with a definitely-non-null `T & Any` keeps that `& Any` when it is read
// through a projected receiver: through `Holder<*>` the value is `Any` and `Box<T & Any>` is
// `Box<out Any>`, through `Holder<out Named?>` they are `Named` and `Box<out Named>`, and
// through `Holder<in String?>` the box is `Box<in String>`. A concrete override that narrows the
// members to `String` reads the same way.

class Box<T>(val item: T)

interface Named {
    val name: String
}

class Label(override val name: String) : Named

abstract class Node<T>

abstract class Holder<T> : Node<T>() {
    abstract val value: T & Any
    abstract val box: Box<T & Any>
}

class Text<T>(override val value: T & Any) : Holder<T>() {
    override val box: Box<T & Any> = Box(value)
}

class Word(override val value: String) : Holder<String?>() {
    override val box: Box<String> = Box(value)
}

fun star(holder: Holder<*>): Boolean {
    val value: Any = holder.value
    val box: Box<out Any> = holder.box
    return value == "O" && box.item == "O"
}

fun covariant(holder: Holder<out Named?>): String {
    val value: Named = holder.value
    val box: Box<out Named> = holder.box
    return value.name + box.item.name
}

fun starString(holder: Holder<*>): String = (holder.value as String) + (holder.box.item as String)

fun contravariant(holder: Holder<in String?>): Box<in String> = holder.box

// A safe cast without written type arguments takes them from the cast value: `Holder<*>`.
fun cast(node: Node<*>): Box<out Any>? = (node as? Holder)?.box

fun box(): String {
    val text = Text<String?>("O")
    if (!star(text)) return "star"
    val covariant = covariant(Text<Label?>(Label("K")))
    if (covariant != "KK") return "covariant: $covariant"
    if (contravariant(text).item != "O") return "contravariant"
    val word = Word("O")
    if (!star(word)) return "word star"
    if (starString(word) != "OO") return "word: ${starString(word)}"
    if (contravariant(word).item != "O") return "word contravariant"
    if (cast(word)?.item != "O") return "word cast"
    if (cast(text)?.item != "O") return "cast"
    return "OK"
}
