// MODULE: lib
fun <T> equalsIntLiteral(value: T): Boolean {
    val actual: Any? = value
    val literal: Any = 42
    return actual == literal
}
fun <T> equalsLongLiteral(value: T): Boolean {
    val actual: Any? = value
    val literal: Any = 42L
    return actual == literal
}
// MODULE: main(lib)
fun box(): String {
    if (!equalsIntLiteral(42)) return "Int box identity"
    if (!equalsLongLiteral(42L)) return "Long box identity"
    if (equalsIntLiteral(42L) || equalsLongLiteral(42)) return "mixed primitive identities"
    return "OK"
}
