class Column<T>(val name: String)

class ColumnSet(private val only: Column<out Any>) {
    operator fun contains(column: Column<out Any>): Boolean = column === only
}

operator fun <T> ColumnSet.contains(column: T): Boolean = column != null

fun <R> originalColumn(column: Column<R>): Column<R>? = column

fun <R> id(value: R): R = value

class Registry {
    fun has(name: String): String = "member"
}

fun Registry.has(name: String?): String = "extension"

fun isId(column: Column<*>, columns: ColumnSet): Boolean = originalColumn(column) in columns

fun select(text: String?, registry: Registry): String = registry.has(id(text))

fun box(): String {
    val id = Column<Int>("id")
    val columns = ColumnSet(id)
    if (!isId(id, columns)) return "id column"
    if (!isId(Column<String>("name"), columns)) return "extension contains"
    val missing: Column<*>? = null
    if (id(missing) in columns) return "nullable in"
    if (select("x", Registry()) != "extension") return "extension"
    if (Registry().has(id("x")) != "member") return "member"
    return "OK"
}
