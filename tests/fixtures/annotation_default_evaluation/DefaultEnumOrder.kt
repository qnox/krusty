object Events {
    var log: String = ""
}

enum class WrittenFirst {
    VALUE;
    init { Events.log += "W" }
}

enum class DeclaredFirst {
    VALUE;
    init { Events.log += "D" }
}

annotation class OrderedPair(val first: DeclaredFirst, val second: WrittenFirst)

annotation class Envelope(
    val value: OrderedPair = OrderedPair(
        second = WrittenFirst.VALUE,
        first = DeclaredFirst.VALUE,
    ),
)

fun box(): String {
    val envelope = Envelope()
    return Events.log + ":" + envelope.value.first.name + ":" + envelope.value.second.name
}

fun main() {
    println(box())
}
