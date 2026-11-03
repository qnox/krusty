// A type parameter's supertypes are its upper bounds, so joining it with another type reaches the
// shared supertype through the bound, and a bound that admits null keeps the join nullable.

open class Base(val tag: String)
class Sub(tag: String) : Base(tag)

class Two<out E>(val first: E, val second: E)

fun <E> two(first: E, second: E): Two<E> = Two(first, second)

fun <T : Base> boundedJoin(t: T): Two<Base> = two(t, Sub("s"))

fun <T : Base?> nullableBoundJoin(t: T): Two<Base?> = two(t, Sub("s"))

fun <T : Base, U : T> chainedBoundJoin(u: U): Two<Base> = two(u, Sub("s"))

fun <T : Base> inferredJoin(t: T) = two(t, Sub("s"))

open class Expr<V>
class Col<V> : Expr<V>()
class Key<K : Any>(val raw: K)
open class InList<V>(val expr: Expr<out V>, val first: V)
class SingleInList<V>(expr: Expr<out V>, first: V) : InList<V>(expr, first)

fun <K : Any, ID : Key<K>?> Col<ID>.inList(raw: K): InList<Key<K>?> = SingleInList(this, Key(raw))

fun box(): String {
    val bounded = boundedJoin(Base("b"))
    if (bounded.first.tag + bounded.second.tag != "bs") return "bounded"
    val nullable = nullableBoundJoin<Base?>(null)
    if (nullable.first != null || nullable.second?.tag != "s") return "nullable"
    val chained = chainedBoundJoin<Base, Sub>(Sub("c"))
    if (chained.first.tag + chained.second.tag != "cs") return "chained"
    val inferred: Two<Base> = inferredJoin(Sub("i"))
    if (inferred.first.tag != "i") return "inferred"
    val op = Col<Key<String>>().inList("k")
    if (op.first?.raw != "k") return "inList"
    return "OK"
}
