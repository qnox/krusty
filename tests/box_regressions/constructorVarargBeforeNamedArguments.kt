class Statement(val table: String, vararg val keys: String, val exclude: String?, val limit: Int)

class Holder {
    fun spread(vararg keys: String): Statement = Statement("t", *keys, exclude = "x", limit = 2)

    fun named(vararg keys: String): Statement = Statement(table = "t", keys = keys, exclude = null, limit = 3)

    fun elements(): Statement = Statement("t", "a", "b", exclude = "y", limit = 4)

    fun none(): Statement = Statement("t", exclude = null, limit = 5)
}

fun describe(statement: Statement): String =
    statement.table + ":" + statement.keys.size + ":" + statement.exclude + ":" + statement.limit

fun box(): String {
    val holder = Holder()
    if (describe(holder.spread("a", "b")) != "t:2:x:2") return "spread"
    if (describe(holder.named("a")) != "t:1:null:3") return "named"
    if (describe(holder.elements()) != "t:2:y:4") return "elements"
    if (describe(holder.none()) != "t:0:null:5") return "none"
    return "OK"
}
