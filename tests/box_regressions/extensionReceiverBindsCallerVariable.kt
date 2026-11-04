// A generic extension receiver applies when it binds to the caller's own type variable.
class EntityID<T : Any>(val value: T)
class IdTable<T : Any>(val name: String)
class Column<T>(val table: IdTable<*>)

@Suppress("UNCHECKED_CAST")
private fun <T : Any, E : EntityID<T>> Column<out E?>.idTable(): IdTable<T> = table as IdTable<T>

fun <T : Comparable<T>> Column<EntityID<T>>.qualified(): String = this.idTable().name
fun <T : Comparable<T>> Column<EntityID<T>?>.implicit(): String = idTable().name
fun <E : EntityID<Int>> Column<E>.bounded(): String = idTable().name

fun box(): String {
    val o = Column<EntityID<Int>>(IdTable<Int>("O")).qualified()
    val k = Column<EntityID<Int>?>(IdTable<Int>("K")).implicit()
    val empty = Column<EntityID<Int>>(IdTable<Int>("")).bounded()
    return o + k + empty
}
