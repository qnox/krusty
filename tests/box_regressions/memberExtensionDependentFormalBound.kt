// A member extension applies only when its solved formals satisfy their declared bounds, as any
// generic call does. `S : T` rejects the `Column<Id<T>>` overload for a `Column<Id<T>>` receiver
// (`S = Id<T>` is not a `T`), although its parameter is the more specific one, and the
// `Column<T>` overload for a `Column<Int>` receiver with an `Id<Int>` reference (`S = Int` is not
// an `Id<Int>`). The overloads return different classes so their JVM signatures differ.
open class Column<T>

class Id<T>

class Plain

class ById

object Table {
    fun <T : Any, S : T, C : Column<S>> C.references(ref: Column<T>): Plain = Plain()

    fun <T : Any, S : T, C : Column<S>> C.references(ref: Column<Id<T>>): ById = ById()

    fun <T : Any> byId(column: Column<Id<T>>, ref: Column<Id<T>>): Any = column.references(ref)

    fun intById(column: Column<Int>, ref: Column<Id<Int>>): Any = column.references(ref)

    fun <T : Any> Column<Id<T>>.implicit(ref: Column<Id<T>>): Any = references(ref)

    fun implicitOn(column: Column<Id<String>>): Any = column.implicit(Column())
}

fun box(): String {
    if (Table.byId(Column<Id<String>>(), Column()) !is Plain) return "Fail by id"
    if (Table.intById(Column(), Column()) !is ById) return "Fail int by id"
    if (Table.implicitOn(Column()) !is Plain) return "Fail implicit"
    return "OK"
}
