// A non-null `x as? T` result smart-casts its stable operand `x` to `T`: after an elvis that leaves
// the function, after an `== null` check that returns, and inside a safe-call chain rooted at the
// cast whose result compares equal to `true`.
class Column(val table: String) {
    fun named(): Boolean = table.length > 0
}

fun afterElvis(value: Any?): Int {
    (value as? String) ?: return -1
    return value.length
}

fun afterNullCheck(value: Any?): Int {
    if ((value as? String) == null) return -1
    return value.length
}

fun inSafeCallChain(value: Any): String {
    if ((value as? Column)?.named() == true && value.table.length == 5) return value.table
    return "none"
}

fun box(): String {
    if (afterElvis("abc") != 3) return "fail elvis"
    if (afterElvis(null) != -1) return "fail elvis miss"
    if (afterNullCheck("ab") != 2) return "fail null check"
    if (afterNullCheck(null) != -1) return "fail null check miss"
    if (inSafeCallChain(Column("users")) != "users") return "fail chain"
    if (inSafeCallChain("users") != "none") return "fail chain miss"
    return "OK"
}
