// A type test on a stable property path keeps the property's declared type beside the tested
// one, as it does for a local: `delegate is Dated` on `val delegate: Column<T>` reads `delegate`
// as `Column<T> & Dated`, so members of both are visible.
interface Column<T> {
    fun sqlType(): String
}

interface Dated {
    val hasTime: Boolean
}

class DateColumn : Column<String>, Dated {
    override fun sqlType(): String = "DATE"
    override val hasTime: Boolean get() = false
}

class Plain(val delegate: Column<String>) {
    fun marker(): String = if (delegate is Dated) delegate.sqlType() + delegate.hasTime else "none"
}

class Generic<T>(val delegate: Column<T>) {
    fun marker(): String = if (delegate is Dated) delegate.sqlType() + delegate.hasTime else "none"
}

class Outer(val inner: Plain)

fun nested(outer: Outer): String =
    if (outer.inner.delegate is Dated) outer.inner.delegate.sqlType() + outer.inner.delegate.hasTime else "none"

val topLevel: Column<String> = DateColumn()

fun topLevelMarker(): String = if (topLevel is Dated) topLevel.sqlType() + topLevel.hasTime else "none"

fun box(): String {
    if (Plain(DateColumn()).marker() != "DATEfalse") return "Fail plain"
    if (Generic(DateColumn()).marker() != "DATEfalse") return "Fail generic"
    if (nested(Outer(Plain(DateColumn()))) != "DATEfalse") return "Fail nested"
    if (topLevelMarker() != "DATEfalse") return "Fail top-level"
    return "OK"
}
