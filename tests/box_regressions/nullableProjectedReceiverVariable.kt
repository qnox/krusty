open class EntityID<T : Any>(val value: T)
class Column<T>(val name: String)
class IdTable<T : Any>(val name: String)

fun <T : Any, E : EntityID<T>> Column<out E?>.idTable(): IdTable<T> = IdTable(name)

fun <U : Any> nonNullTable(column: Column<EntityID<U>>) = column.idTable().name
fun <U : Any> nullableTable(column: Column<EntityID<U>?>) = column.idTable().name
fun concreteTable(column: Column<EntityID<Int>>) = column.idTable().name

fun box(): String {
    if (nonNullTable(Column<EntityID<String>>("a")) != "a") return "fail non-null"
    if (nullableTable(Column<EntityID<Long>?>("b")) != "b") return "fail nullable"
    if (concreteTable(Column<EntityID<Int>>("c")) != "c") return "fail concrete"
    return "OK"
}
