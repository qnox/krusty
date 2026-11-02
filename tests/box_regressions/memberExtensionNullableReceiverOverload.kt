// A member extension declared on `Column<U?>` does not apply to a caller's `Column<A>` with
// `A : Any`: `U? = A` has no solution, so its `Column<U>` overload is selected without ambiguity,
// and the nullable overload is selected for `Column<A?>`. The overloads return different types so
// their JVM signatures differ without a platform-specific name.
class Column<T>

interface Transformer<U, W>

class Pair<U, W> : Transformer<U, W>

fun <U, W> transformer(): Transformer<U, W> = Pair()

fun <U, W> columnTransformer(unwrap: (W) -> U, wrap: (U) -> W): Transformer<U, W> = Pair()

object Columns {
    fun <U : Any> Column<U>.kind(): String = "non-null"

    fun <U : Any> Column<U?>.kind(): CharSequence = "nullable"

    fun <U : Any, W : Any> Column<U>.transform(transformer: Transformer<U, W>): String = "non-null"

    fun <U : Any, W : Any> Column<U?>.transform(transformer: Transformer<U?, W?>): CharSequence = "nullable"

    fun <A : Any> generic(column: Column<A>): String = column.kind()

    fun <A : Any> nullableGeneric(column: Column<A?>): CharSequence = column.kind()

    fun concrete(column: Column<String>): String = column.kind()

    fun <A : Any> Column<A>.implicit(): String = kind()

    fun <A : Any, B : Any> transformed(column: Column<A>): String = column.transform(transformer<A, B>())

    fun <A : Any, B : Any> Column<A>.mapped(wrap: (A) -> B, unwrap: (B) -> A): String =
        transform(columnTransformer(unwrap, wrap))

    fun implicitOn(column: Column<Int>): String = column.implicit()

    fun mappedOn(column: Column<Int>): String = column.mapped({ it + 1 }, { it - 1 })
}

fun box(): String {
    if (Columns.generic(Column<Int>()) != "non-null") return "Fail generic"
    if (Columns.nullableGeneric(Column<Int?>()) != "nullable") return "Fail nullable generic"
    if (Columns.concrete(Column()) != "non-null") return "Fail concrete"
    if (Columns.implicitOn(Column()) != "non-null") return "Fail implicit"
    if (Columns.transformed<Int, String>(Column()) != "non-null") return "Fail transform"
    if (Columns.mappedOn(Column()) != "non-null") return "Fail mapped"
    return "OK"
}
