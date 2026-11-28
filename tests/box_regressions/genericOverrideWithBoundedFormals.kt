// An override of a generic member matches its slot after the supertype's formals are bound to the
// subclass's own formals, including bounds that refer to the formals themselves (`T : Comparable<T>`)
// and to another formal of the same class (`T : List<U>`).
abstract class Ordered<T : Comparable<T>> {
    abstract fun pick(items: List<T>): T
}

class Largest<S : Comparable<S>> : Ordered<S>() {
    override fun pick(items: List<S>): S = items.maxOrNull()!!
}

abstract class Grouped<U, T : List<U>> {
    abstract fun first(group: T, index: Map<U, T>): U
}

class Groups<E : CharSequence> : Grouped<E, List<E>>() {
    override fun first(group: List<E>, index: Map<E, List<E>>): E = group.first { index.containsKey(it) }
}

fun box(): String {
    val ordered: Ordered<String> = Largest()
    if (ordered.pick(listOf("a", "c", "b")) != "c") return "self-referencing bound"
    val grouped: Grouped<String, List<String>> = Groups()
    if (grouped.first(listOf("x", "y"), mapOf("y" to listOf("y"))) != "y") return "bound on another formal"
    return "OK"
}
