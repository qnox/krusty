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

fun box(): String {
    val value = OrderedPair(
        second = WrittenFirst.VALUE,
        first = DeclaredFirst.VALUE,
    )
    return Events.log + ":" + value.first.name + ":" + value.second.name
}

fun main() {
    println(box())
}
