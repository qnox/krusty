// A formal bounded by another formal's nullable type (`S : T?`) receives only `X & Any` from a
// lower bound `X` of `S`, so a caller's `S` does not widen the callee's `T` past the caller's own
// `T`, which an invariant position already fixes. The caller's `T` is not itself a subtype of
// `S & Any`, even when both share the classifier bound `CharSequence`.
open class Expression<T>(val label: String)

class Column<T>(val name: String)

fun <T, S : T?> update(column: Column<T>, value: Expression<S>): String = column.name + value.label

fun <T, S : T?> direct(column: Column<T>, value: Expression<S>): String = update(column, value)

class Builder<T>(val column: Column<T>) {
    fun <S : T?> set(value: Expression<S>): String = update(column, value)
}

fun <T : CharSequence, S : T?> bounded(column: Column<T>, value: Expression<S>): String = update(column, value)

fun box(): String {
    val column = Column<String>("O")
    val first = direct(column, Expression<String?>("K"))
    val second = Builder(column).set(Expression<String>("K"))
    val third = bounded(column, Expression<Nothing?>("K"))
    return if (first == "OK" && second == "OK" && third == "OK") "OK" else "fail: $first $second $third"
}
