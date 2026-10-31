// A caller's type variable bounded by a nullable type binds a nullable formal of a projected
// extension receiver to its definitely non-null form, and the receiver's other formal reads its
// type from the caller variable's bound.
open class EntityID<T : Any>(val value: T)
class Column<T>(val name: String)
class IdTable<T : Any>(val name: String)

fun <T : Any, E : EntityID<T>> Column<out E?>.idTable(): IdTable<T> = IdTable(name)

fun <V : EntityID<String>?> boundedTable(column: Column<V>) = column.idTable().name

fun box(): String {
    if (boundedTable(Column<EntityID<String>?>("a")) != "a") return "fail nullable argument"
    if (boundedTable(Column<EntityID<String>>("b")) != "b") return "fail non-null argument"
    return "OK"
}
