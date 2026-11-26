class Item(val text: String)
class Sink {
    var taken = ""
    fun take(item: Item): Int { taken += item.text; return taken.length }
}
fun find(text: String): Item? = Item(text)
fun Sink.takeVia(item: Item): Int = take(item)

fun box(): String {
    val sink = Sink()
    val first = find("O")
    sink.take(first!!)
    val second = find("K")
    sink.takeVia(second!!)
    return first.text + second.text
}
