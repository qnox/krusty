// A formal that occurs only in an `in` projection of the receiver is constrained from above:
// `Expr<T>` against `Expr<in S>` says `S <: T`. Its declared bound `S : T?` is one more upper
// constraint, so the call solves `S` at their intersection instead of rejecting `S = T`.
open class Expr<T>(val label: String)

class Param<T>(val value: T)

fun <T, S : T?> Expr<in S>.wrap(value: T): Param<T> = Param(value)

fun <T : CharSequence?, S : T> Expr<T>.param(t: S): Param<S> = wrap(t)

fun <T : CharSequence?, S : T> Expr<T>.explicitParam(t: S): Param<S> = this.wrap(t)

fun <T : CharSequence?, S : T> plainParam(expr: Expr<T>, t: S): Param<S> = expr.wrap(t)

fun box(): String {
    val nullable = Expr<String?>("nullable").param("O")
    val nonNull = Expr<CharSequence>("non-null").explicitParam("K")
    val plain = plainParam(Expr<String>("plain"), "!")
    val result = nullable.value + nonNull.value + plain.value
    return if (result == "OK!") "OK" else "fail: $result"
}
