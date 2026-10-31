// A destination producer (`filterInto(bagOf(), …)`) is solved against the call's expected
// result when the receiver already fixed the element type: `C : Sink<in Sub>` admits the expected
// `Bag<Base>`, so the producer becomes `bagOf<Base>()`.
open class Base(val name: String)
class Sub(name: String) : Base(name)

interface Sink<T> {
    fun put(item: T)
}

class Bag<T : Base> : Sink<T> {
    var count = 0
    var names = ""

    override fun put(item: T) {
        count += 1
        names += item.name
    }
}

fun <T : Base> bagOf(): Bag<T> = Bag()

class Source<T>(val first: T, val second: T, val third: T)

fun <T, C : Sink<in T>> Source<T>.filterInto(destination: C, skip: Any?): C {
    if (first != skip) destination.put(first)
    if (second != skip) destination.put(second)
    if (third != skip) destination.put(third)
    return destination
}

fun <T, C : Sink<in T>> Source<T>.copyTo(destination: C): C = filterInto(destination, null)

fun <T, C : Sink<in T>> into(xs: Source<T>, destination: C): C = xs.copyTo(destination)

fun filtered(xs: Source<Sub>, skip: Sub): Bag<Base> = xs.filterInto(bagOf(), skip)

fun returned(xs: Source<Sub>): Bag<Base> {
    return xs.filterInto(bagOf(), null)
}

fun initialized(xs: Source<Sub>): Bag<Base> {
    val result: Bag<Base> = xs.copyTo(bagOf())
    return result
}

fun topLevel(xs: Source<Sub>): Bag<Base> = into(xs, bagOf())

fun box(): String {
    val skip = Sub("skip")
    val xs = Source(Sub("a"), skip, Sub("b"))
    val bag = filtered(xs, skip)
    bag.put(Base("c"))
    if (bag.names != "abc") return "filtered: ${bag.names}"
    if (returned(xs).count != 3) return "returned"
    if (initialized(xs).count != 3) return "initialized"
    if (topLevel(xs).count != 3) return "topLevel"
    return "OK"
}
