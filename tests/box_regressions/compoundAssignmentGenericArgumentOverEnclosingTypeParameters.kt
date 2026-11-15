// A nested generic call whose result mentions only the enclosing declaration's own type
// parameters (`Entry(Pair(r, d))` inside `fun <R, D>`) is as determined as a concrete one, so it
// still selects a generic `plusAssign` whose parameter it must bind.
interface Property<T> {
    val value: T
}

class Pair<A, B>(val first: A, val second: B)

class Entry<R, D>(override val value: Pair<R, D>) : Property<Pair<R, D>>

class Holder<T>(val value: T)

class Box<A>(override val value: Holder<A>) : Property<Holder<A>>

class Container<T> {
    var count = 0
    var last: Any? = null

    operator fun <V : T> plusAssign(property: Property<V>) {
        count++
        last = property.value
    }
}

class Attributes {
    val container = Container<Any>()

    fun of(key: String): Container<Any> = container
}

fun <R, D> register(attributes: Attributes, reference: R, definition: D) {
    attributes.of("key") += Entry(Pair(reference, definition))
}

fun <R> wrap(container: Container<Any>, value: R) {
    container += Box(Holder(value))
}

fun box(): String {
    val attributes = Attributes()
    register(attributes, "O", 1)
    val pair = attributes.container.last as Pair<*, *>
    if (pair.first != "O" || pair.second != 1) return "fail pair"
    wrap(attributes.container, "K")
    val holder = attributes.container.last as Holder<*>
    if (holder.value != "K") return "fail holder"
    if (attributes.container.count != 2) return "fail count"
    return "OK"
}
