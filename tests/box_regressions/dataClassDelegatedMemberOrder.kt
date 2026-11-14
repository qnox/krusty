// A data class that delegates an interface lists its generated equals/hashCode/toString and its
// delegation forwarders in one name-sorted run of its metadata, after its own members, componentN
// and copy.
interface Greeter {
    fun greet(): String
    fun answer(times: Int): Int
    val size: Int
}

class Base : Greeter {
    override fun greet(): String = "O"
    override fun answer(times: Int): Int = 21 * times
    override val size: Int get() = 1
    override fun toString(): String = "Base"
    override fun hashCode(): Int = 7
}

data class Named(val greeter: Greeter, val suffix: String) : Greeter by greeter {
    fun own(): String = suffix
}

fun box(): String {
    val base = Base()
    val named = Named(base, "K")
    if (named.size != 1) return "size ${named.size}"
    if (named.answer(2) != 42) return "answer ${named.answer(2)}"
    if (named != Named(base, "K")) return "equals"
    if (named.hashCode() != Named(base, "K").hashCode()) return "hashCode"
    if (named.toString() != "Named(greeter=Base, suffix=K)") return "toString $named"
    return named.greet() + named.own()
}
