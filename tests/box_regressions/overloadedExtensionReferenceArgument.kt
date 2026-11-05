// An overloaded extension referenced as an argument is selected by the parameter it is passed to:
// `Op<Boolean>::and` fits `(Op<Boolean>, Op<Boolean>) -> Op<Boolean>` only through
// `and(Expr<Boolean>)`. The call's candidate comes from every scope-tower level first, including
// the extensions on an implicit receiver, and the reference keeps its written type arguments.
// Each candidate is one constraint system: `pick(Int): String` cannot satisfy `(Receiver, T) -> T`,
// so only `pick(Byte): Byte` fits and the soft literal `1` becomes a `Byte`.

open class Expr<T>(val label: String)
class Op<T>(label: String) : Expr<T>(label)

infix fun Expr<Boolean>.and(op: Expr<Boolean>): Op<Boolean> = Op("(" + label + " and " + op.label + ")")

inline fun Expr<Boolean>.and(op: () -> Op<Boolean>): Op<Boolean> = and(op())

class Chain<out T>(val first: T, val second: T, val third: T)

fun <S, T : S> Chain<T>.combine(operation: (S, T) -> S): S =
    operation(operation(first, second), third)

fun <S, T : S> fold(first: T, second: T, operation: (S, T) -> S): S = operation(first, second)

fun Chain<Op<Boolean>>.implicitReceiver(): Op<Boolean> = combine(Op<Boolean>::and)

fun Chain<Op<Boolean>>.explicitReceiver(): Op<Boolean> = this.combine(Op<Boolean>::and)

fun Chain<Op<Boolean>>.inferredImplicitReceiver() = combine(Op<Boolean>::and)

fun inferredExplicitReceiver(chain: Chain<Op<Boolean>>) = chain.combine(Op<Boolean>::and)

fun inferredTopLevel(first: Op<Boolean>, second: Op<Boolean>) = fold(first, second, Op<Boolean>::and)

class Conditions(private val chain: Chain<Op<Boolean>>) {
    private fun combineAll(operation: (Op<Boolean>, Op<Boolean>) -> Op<Boolean>): Op<Boolean> =
        chain.combine(operation)

    fun all(): Op<Boolean> = combineAll(Op<Boolean>::and)
}

class Receiver

fun Receiver.pick(value: Int): String = "wrong"

fun Receiver.pick(value: Byte): Byte = value

fun <T> choose(seed: T, operation: (Receiver, T) -> T): T = operation(Receiver(), seed)

fun box(): String {
    val chain = Chain(Op<Boolean>("a"), Op("b"), Op("c"))
    val expected = "((a and b) and c)"
    if (chain.implicitReceiver().label != expected) return "implicit receiver"
    if (chain.explicitReceiver().label != expected) return "explicit receiver"
    if (chain.inferredImplicitReceiver().label != expected) return "inferred implicit receiver"
    if (inferredExplicitReceiver(chain).label != expected) return "inferred explicit receiver"
    if (inferredTopLevel(chain.first, chain.second).label != "(a and b)") return "inferred top level"
    if (Conditions(chain).all().label != expected) return "member"
    val selected = choose(1, Receiver::pick)
    if (selected != 1.toByte()) return "explicit choice"
    return "OK"
}
