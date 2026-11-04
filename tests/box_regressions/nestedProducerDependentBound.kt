// A nested generic producer passed where the outer variable `S` is still open but bounded by an
// already fixed `T` (`S : T?`) is solved against that bound: `nullableExpr<R : Any>()` produces
// `Expression<R?>`, so `R?` meets `T?` and `R` becomes `T & Any`, not a mismatch against `S`.
open class Expression<T>(val label: String)

class Column<T>(val name: String)

fun <T : Any> nullableExpr(label: String): Expression<T?> = Expression(label)

fun <T, S : T?> update(column: Column<T>, value: Expression<S>): String = column.name + value.label

fun concrete(column: Column<String>): String = update(column, nullableExpr("K"))

fun <S> generic(column: Column<S>): String = update(column, nullableExpr("K"))

fun <S> inferred(column: Column<S>) = update(column, nullableExpr("K"))

fun box(): String {
    val column = Column<String>("O")
    val results = concrete(column) + generic(column) + inferred(column)
    return if (results == "OKOKOK") "OK" else "fail: $results"
}
