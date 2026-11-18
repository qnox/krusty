// WITH_STDLIB
interface INode { val id: Long }
interface Event
class Node : INode {
    override var id: Long = 0
}
class Builder {
    val nodes = ArrayList<INode>()

    @Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")
    @kotlin.internal.LowPriorityInOverloadResolution
    fun add(element: INode): Boolean = nodes.add(element)

    fun add(event: Event): Builder = this

    fun record(value: Long): Builder =
        apply {
            add(Node().apply {
                id = value
            })
        }
}

fun box(): String {
    val b = Builder().record(7L)
    return if (b.nodes.size == 1 && b.nodes[0].id == 7L) "OK" else "FAIL"
}
