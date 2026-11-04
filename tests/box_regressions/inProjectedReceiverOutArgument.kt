open class Expr<T>(val v: T)
class Param<T>(val v: T)
fun <T, S : T?> Expr<in S>.wrap(value: T): Param<T> = Param(value)
fun <S : CharSequence> Expr<in S>.tag(): String = "K"
fun outProjected(c: Expr<out Any>, x: String): String = c.wrap(x).v
fun star(c: Expr<*>): String = c.tag()
fun box(): String = outProjected(Expr("x"), "O") + star(Expr(""))
