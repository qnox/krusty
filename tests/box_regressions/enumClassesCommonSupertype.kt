// Two enum classes join at `Enum<*>`: the recursive bound `E : Enum<E>` leads `Enum<A>` and
// `Enum<B>` back to the join of `A` and `B`, which kotlinc cuts at a star. With a shared interface
// the join is `Enum<*> & Factory`, so both the interface and the `Enum` members are reachable.

fun interface Factory {
    fun create(): String
}

enum class First : Factory {
    O;

    override fun create() = name
}

enum class Second : Factory {
    K;

    override fun create() = name
}

enum class Plain { A }

enum class Other { B }

class Box<out T>(val value: T)

fun <T> either(first: T, second: T, takeFirst: Boolean): T = if (takeFirst) first else second

fun box(): String {
    val factory = either(First.O, Second.K, false)
    if (factory.create() != "K" || factory.ordinal != 0 || factory.name != "K") return "fail: factory"

    val plain = either(Plain.A, Other.B, true)
    if (plain.name != "A" || plain.ordinal != 0) return "fail: plain"

    val joined: Factory = either(First.O, Second.K, true)
    if (joined.create() != "O") return "fail: joined"

    val boxed = either(Box(First.O), Box(Second.K), false)
    if (boxed.value.create() != "K" || boxed.value.ordinal != 0) return "fail: boxed"
    return "OK"
}
