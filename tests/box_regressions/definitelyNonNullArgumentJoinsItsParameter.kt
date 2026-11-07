// A constructor formal `T` that takes both a caller's `T` and that caller's `T & Any` joins them to
// `T`, not `Any`, so another argument whose formal is bounded by `T` still fits.
interface ColumnType<T>

open class Expression<T>

open class Typed<T>(val columnType: ColumnType<T & Any>) : Expression<T>()

class Param<T>(val value: T, type: ColumnType<T & Any>) : Expression<T>()

class Plus<T, S : T>(val left: Expression<T>, val right: Expression<S>, val columnType: ColumnType<T & Any>)

fun <T> Typed<T>.wrap(value: T): Param<T> = Param(value, columnType)

fun <T> Typed<T>.plusValue(value: T): Plus<T, T> = Plus(this, wrap(value), columnType)

fun <T, S : T> Typed<T>.plusExpression(other: Expression<S>): Plus<T, S> = Plus(this, other, columnType)

object IntType : ColumnType<Int>

fun box(): String {
    val column = Typed<Int?>(IntType)
    val value = column.plusValue(1)
    if (value.left !== column || value.columnType !== IntType) return "Fail value"
    val other = Typed<Int>(IntType)
    val expression = column.plusExpression(other)
    if (expression.right !== other || expression.columnType !== IntType) return "Fail expression"
    return "OK"
}
