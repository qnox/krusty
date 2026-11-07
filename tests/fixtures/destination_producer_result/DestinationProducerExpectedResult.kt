// A destination producer (`filterTo(mutableSetOf())`) is solved against the call's expected result
// when the receiver already fixed the element type: `C : MutableCollection<in Sub>` admits the
// expected `MutableSet<Base>`, so the producer becomes `mutableSetOf<Base>()`.
open class Base(val name: String)
class Sub(name: String) : Base(name)

fun <T, C : MutableCollection<in T>> into(xs: List<T>, destination: C): C {
    destination.addAll(xs)
    return destination
}

fun filtered(xs: List<Sub>): MutableSet<Base> = xs.filterTo(mutableSetOf()) { it.name != "skip" }

fun returned(xs: List<Sub>): MutableSet<Base> {
    return xs.filterTo(mutableSetOf()) { true }
}

fun initialized(xs: List<Sub>): MutableSet<Base> {
    val result: MutableSet<Base> = xs.toCollection(mutableSetOf())
    return result
}

fun mapped(xs: List<Sub>): MutableList<Base> = xs.mapTo(mutableListOf()) { it }

fun topLevel(xs: List<Sub>): MutableSet<Base> = into(xs, mutableSetOf())

fun box(): String {
    val xs = listOf(Sub("a"), Sub("skip"), Sub("b"))
    val set = filtered(xs)
    set.add(Base("c"))
    if (set.map { it.name } != listOf("a", "b", "c")) return "filtered"
    if (returned(xs).size != 3) return "returned"
    if (initialized(xs).size != 3) return "initialized"
    val list = mapped(xs)
    list.add(Base("d"))
    if (list.size != 4) return "mapped"
    if (topLevel(xs).size != 3) return "topLevel"
    return "OK"
}
