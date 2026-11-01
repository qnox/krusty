// An invariant argument at a `T & Any` position fixes `T` only up to nullability: `Col<String>`
// admits `T = String?`, which another argument (`Fn<String?>`) or the expected type then fixes.
interface Col<T : Any> {
    fun name(): String
}

abstract class Expr<T> {
    abstract val columnType: Col<T & Any>
}

interface Fn<T> {
    fun over(): Def<T>
}

class Def<T>(val columnType: Col<T & Any>, val function: Fn<T>)

class Lag<T>(val expr: Expr<T>) : Fn<T?> {
    override fun over(): Def<T?> {
        return Def(expr.columnType, this)
    }
}

class Named(private val label: String) : Col<String> {
    override fun name() = label
}

class Text(label: String) : Expr<String>() {
    override val columnType = Named(label)
}

class Pair2<T>(val function: Fn<T>, val columnType: Col<T & Any>)

fun explicit(function: Fn<String?>, column: Col<String>): Pair2<String?> =
    Pair2<String?>(function, column)

fun inferred(function: Fn<String?>, column: Col<String>) = Pair2(function, column)

fun box(): String {
    val lag = Lag(Text("O"))
    val definition = lag.over()
    if (definition.function !== lag) return "function"
    val first = explicit(lag, Named("K"))
    val second = inferred(lag, Named("K"))
    if (first.function !== lag || second.function !== lag) return "pair"
    return definition.columnType.name() + second.columnType.name()
}
