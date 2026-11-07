// A member extension's formal observable only through another formal's bound (`T` of
// `E : Entity<T>`) is solved from the receiver's applied bound, as for a top-level extension,
// instead of falling back to its own bound.
class Column<T>(val name: String)
class Table<T : Any>(val name: String)
open class Entity<T : Any>(val id: T)

class Tables {
    fun <T : Any, E : Entity<T>> Column<out E>.table(): Table<T> = Table(name)

    fun <U : Any> Column<Entity<U>>.ownTable(): Table<U> = table()

    fun <U : Any> Column<Entity<U>>.explicitTable(): Table<U> = this.table()

    fun <U : Any> entity(column: Column<Entity<U>>, id: U): Entity<U> {
        val table: Table<U> = column.table()
        return if (table.name == column.name) Entity(id) else Entity(id)
    }
}

fun box(): String {
    val tables = Tables()
    val column = Column<Entity<String>>("users")
    with(tables) {
        if (column.ownTable().name != "users") return "fail own"
        if (column.explicitTable().name != "users") return "fail explicit"
    }
    if (tables.entity(column, "a").id != "a") return "fail entity"
    return "OK"
}
