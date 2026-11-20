// A callable reference to a member of a builder's receiver contributes its constraints to the
// builder's inference: `::add` passed as `(String) -> Unit` inside a builder lambda fixes the
// element type to `String`, exactly as a call `add("x")` would.

class Items<E> {
    var joined = ""
    var count = 0

    fun add(item: E): Boolean {
        joined += item
        count++
        return true
    }
}

class Collector(val sink: (String) -> Unit)

fun <E> build(body: Items<E>.() -> Unit): Items<E> {
    val items = Items<E>()
    items.body()
    return items
}

fun collect(gen: Collector.() -> Unit): Items<String> = build { Collector(::add).gen() }

fun withBoolean(feed: (String) -> Boolean): Boolean = feed("x")

fun box(): String {
    val collected = collect {
        sink("O")
        sink("K")
    }
    if (collected.count != 2) return "fail: collected"
    val bound = build { withBoolean(this::add) }
    val boundValues: Items<String> = bound
    if (boundValues.count != 1 || boundValues.joined != "x") return "fail: bound"
    return collected.joined
}
