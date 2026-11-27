// An interface that redeclares `kotlin.Any`'s members abstractly hands them to `I by d`: the
// forwarders dispatch to the delegate through `equals`, `hashCode` and `toString`.

interface Keyed {
    override fun equals(other: Any?): Boolean
    override fun hashCode(): Int
    override fun toString(): String
}

class Key(val id: Int) : Keyed {
    override fun equals(other: Any?): Boolean = other is Keyed && other.hashCode() == id
    override fun hashCode(): Int = id
    override fun toString(): String = "Key($id)"
}

class Wrapped(id: Int) : Keyed by Key(id)

fun box(): String {
    val a: Any = Wrapped(7)
    val b: Any = Wrapped(7)
    if (a != b) return "Fail equals"
    if (a == Wrapped(8)) return "Fail distinct"
    if (a.hashCode() != 7) return "Fail hashCode ${a.hashCode()}"
    if (a.toString() != "Key(7)") return "Fail toString $a"
    return "OK"
}
