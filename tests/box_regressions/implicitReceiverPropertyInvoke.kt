class Node(val id: Int)
class Edge<S>(val value: S)

val Int.edge: Edge<Node> get() = Edge(Node(this))
operator fun <S> Edge<S>.invoke(action: (S) -> Unit) = action(value)

fun box(): String {
  var seen = -1
  with(7) { edge { node -> seen = node.id } }
  return if (seen == 7) "OK" else "fail $seen"
}
