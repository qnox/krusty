// A function's own type parameter shadows a same-named type parameter of its class, also inside the
// bounds of the function's other type parameters: in `fun <T, S : T?>` declared in `Builder<T>`,
// `S` is bounded by the function's `T`. The inferred result of `set` needs `update` to accept
// `Expression<S>` with that bound.

open class Expression<T>(val label: String)
class Column<T>(val name: String)

abstract class Builder<out T> {
    var log = ""

    open operator fun <T, S : T?, E : Expression<S>> set(column: Column<T>, value: E) = update(column, value)

    open fun <T, S : T?> update(column: Column<T>, value: Expression<S>) {
        log = log + column.name + "=" + value.label + ";"
    }

    open fun <T, S : T?> update(column: Column<T>, value: () -> Expression<S>) {
        update(column, value())
    }

    fun <T, S : T> pick(value: S, fallback: T): T = if (log == "") fallback else value
}

class Insert : Builder<Int>()

fun box(): String {
    val insert = Insert()
    insert[Column<String>("name")] = Expression<String?>("x")
    insert.update(Column<Int>("id")) { Expression<Int>("1") }
    if (insert.log != "name=x;id=1;") return insert.log
    if (insert.pick("a", "b") != "a") return "pick"
    return "OK"
}
