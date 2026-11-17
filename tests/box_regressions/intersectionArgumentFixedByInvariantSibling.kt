interface Column<T>
class CharColumn : Column<Char>
class StringColumn : Column<String>

class Literal<T>(val column: Column<T & Any>, val value: T) {
    fun describe(): String = value.toString()
}

fun <X> pair(column: Column<X>, value: X): String = value.toString()

fun <T : Any> literal(value: T): String {
    val result: Literal<out Any> = when (value) {
        is Char -> Literal(CharColumn(), value)
        is String -> Literal(StringColumn(), value)
        else -> throw IllegalArgumentException("unsupported")
    }
    return result.describe()
}

fun <T : Any> viaFunction(value: T): String {
    if (value is Char) return pair(CharColumn(), value)
    if (value is String) return pair(StringColumn(), value)
    return "other"
}

fun <T : Any> flipped(value: T): String =
    if (value is Char) flip(value, CharColumn()) else "other"

fun <X> flip(value: X, column: Column<X>): String = value.toString()

fun box(): String {
    if (literal('O') != "O") return "char"
    if (literal("K") != "K") return "string"
    if (viaFunction('a') != "a" || viaFunction("b") != "b" || viaFunction(1) != "other") return "function"
    if (flipped('c') != "c" || flipped(2) != "other") return "flipped"
    return "OK"
}
