class WideConstructor(val value: Any) {
    override fun hashCode(): Int = 0
}

data class ConstructorView(val function: Any) {
    override fun toString(): String = "ConstructorView"
}

fun box(): String {
    val wide: (Any) -> WideConstructor = ::WideConstructor
    val narrow: (String) -> WideConstructor = ::WideConstructor
    if (wide(7).value != 7) return "wide invoke"
    if (narrow("OK").value != "OK") return "narrow invoke"
    val first = ConstructorView(wide)
    val second = ConstructorView(narrow)
    if (first != second) return "declared signature equality"
    if (first.hashCode() != second.hashCode()) return "declared signature hash"
    return "OK"
}
