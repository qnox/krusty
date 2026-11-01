open class Expr<T>(val v: T)
interface I { fun <T> wrap(e: Expr<T>): Expr<T> = e }
open class P {
    open fun <T> id(x: T): T = x
    open fun <T : CharSequence?> len(e: Expr<T>, prefix: String = "<"): String = prefix + (if (e.v == null) "-" else "a")
}
class Q : P(), I {
    override fun <U : CharSequence?> len(e: Expr<U>, prefix: String): String = super.len(e, prefix + "[")
    override fun <T> wrap(e: Expr<T>): Expr<T> = super.wrap(e)
    fun named(): String = super.id<String>("K")
    fun inferred(): String = super.id("O")
}
fun box(): String {
    val q = Q()
    if (q.len(Expr("a"), "") != "[a") return "len"
    if (q.wrap(Expr("w")).v != "w") return "wrap"
    return q.inferred() + q.named()
}
