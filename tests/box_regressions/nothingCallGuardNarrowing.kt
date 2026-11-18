class Tx
class Col(val name: String)
class Key(val only: Col?)

fun fail(message: String): Nothing = throw IllegalStateException(message)
fun Tx.failWith(message: String): Nothing = throw IllegalStateException(message)

fun top(x: String?): Int {
    if (x == null) fail("top")
    return x.length
}

fun block(x: String?): Int {
    if (x == null) {
        fail("block")
    }
    return x.length
}

fun library(x: String?): Int {
    if (x == null) error("library")
    return x.length
}

fun Tx.extension(x: String?): Int {
    if (x == null) failWith("extension")
    return x.length
}

fun Tx.identity(column: Col): String = column.name

fun Tx.merge(a: Key?, b: Key?): String {
    val target = a?.only
    val source = b?.only
    if (target == null || source == null) {
        failWith("MERGE requires an ON condition to be specified.")
    }
    return "${identity(target)}=${identity(source)}"
}

fun chain(x: String?, y: Any): Int {
    if (x == null) fail("x") else if (y !is String) fail("y")
    return x.length + y.length
}

fun box(): String {
    val tx = Tx()
    if (top("ab") != 2 || block("abc") != 3 || library("a") != 1) return "calls"
    if (with(tx) { extension("abcd") } != 4) return "extension"
    if (tx.merge(Key(Col("O")), Key(Col("K"))) != "O=K") return "merge"
    if (chain("a", "bc") != 3) return "chain"
    try {
        top(null)
        return "no throw"
    } catch (e: IllegalStateException) {
        if (e.message != "top") return "message"
    }
    return "OK"
}
