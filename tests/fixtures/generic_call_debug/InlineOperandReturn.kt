open class Base(val name: String)
class Sub(name: String) : Base(name)

fun <T, C : MutableCollection<in T>> into(xs: List<T>, destination: C): C {
    destination.addAll(xs)
    return destination
}

fun expression(xs: List<Sub>): MutableSet<Sub> = into(xs, mutableSetOf())

fun returned(xs: List<Sub>): MutableSet<Sub> {
    return into(xs, mutableSetOf())
}

fun initialized(xs: List<Sub>): MutableSet<Sub> {
    val destination = mutableSetOf<Sub>()
    return into(xs, destination)
}

fun concrete(xs: List<Sub>, destination: MutableSet<Sub>): MutableSet<Sub> {
    destination.addAll(xs)
    return destination
}

fun concreteCall(xs: List<Sub>): MutableSet<Sub> = concrete(xs, mutableSetOf())

fun box(): String {
    val xs = listOf(Sub("a"), Sub("b"))
    for (result in listOf(expression(xs), returned(xs), initialized(xs), concreteCall(xs))) {
        if (result.map { it.name } != listOf("a", "b")) return "contents"
        if (result.first() !== xs.first()) return "identity"
    }
    if (reordered(xs).toList() != xs) return "reordered"
    if (providedCall(xs).toList() != xs) return "provided"
    if (compiledCall(xs) != xs) return "compiled"
    return "OK"
}

fun reordered(xs: List<Sub>): MutableSet<Sub> = into(
    destination = mutableSetOf(),
    xs = xs,
)

inline fun provide(): MutableSet<Sub> = mutableSetOf()

fun providedCall(xs: List<Sub>): MutableSet<Sub> = into(xs, provide())

fun <T> same(value: T): T = value

fun compiledCall(xs: List<Sub>): List<Sub> = same(xs.map { it })
