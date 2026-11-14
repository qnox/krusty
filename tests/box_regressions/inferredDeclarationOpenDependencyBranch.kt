fun pick(c: Boolean) = if (c) emptyMap() else mapOf("O" to "K")
fun pickLast(c: Boolean) = if (c) mapOf(1 to "K") else emptyMap()
fun faced(c: Boolean) = if (c) emptyList() else mutableListOf("O")
val graph = listOf("O").let { keys ->
    if (keys.isEmpty()) emptyMap() else keys.associateWith { "K" }
}
fun box(): String {
    if (pick(false).getValue("O") != "K") return "pick"
    if (pickLast(true).getValue(1) != "K") return "last"
    val first: String = faced(false)[0]
    return first + graph.getValue("O")
}
