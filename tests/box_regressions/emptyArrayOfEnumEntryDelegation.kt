// WITH_STDLIB
// FILE: EnumEntryVarargArray.kt

class Expr<T>

val values = arrayOf<Expr<*>>(Expr<Int>(), Expr<String>())

enum class Kind(vararg val parts: Expr<*>) {
    EMPTY(*arrayOf()),
    NAMED(parts = arrayOf()),
    VALUES(parts = values),
}

fun box(): String {
    if (Kind.EMPTY.parts.size != 0 || Kind.NAMED.parts.size != 0) return "Fail empty"
    if (Kind.VALUES.parts.size != 2) return "Fail values"
    return "OK"
}
