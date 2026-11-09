// Inference over a type parameter whose upper bound names itself (`out T : Out<T>`): solving
// `nested.copy(star)` on `Out<Out<*>>` binds `T` to a type that names `T` again, and applying that
// binding must expand it once rather than re-substitute it forever.
interface Out<out T : Out<T>> {
    fun copy(t: @UnsafeVariance T): Out<T>
}

class Node : Out<Node> {
    override fun copy(t: Node): Out<Node> = t
}

fun copyStar(star: Out<*>, nested: Out<Out<*>>) = nested.copy(star)

fun box(): String {
    val node = Node()
    @Suppress("UNCHECKED_CAST")
    val nested = node as Out<Out<*>>
    return if (copyStar(node, nested) === node) "OK" else "copy returned another instance"
}
