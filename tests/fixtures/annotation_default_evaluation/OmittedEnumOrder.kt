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

annotation class OrderedPair(
    val first: DeclaredFirst = DeclaredFirst.VALUE,
    val second: WrittenFirst,
)

annotation class Envelope(
    val value: OrderedPair = OrderedPair(
        second = WrittenFirst.VALUE,
    ),
)

fun box(): String {
    val envelope = Envelope()
    return Events.log + ":" + envelope.value.first.name + ":" + envelope.value.second.name
}

fun main() {
    println(box())
}
