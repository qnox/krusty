class Constructed(val value: Int) {
    constructor(value: String) : this(value.length)
    override fun hashCode(): Int = value
}

data class ConstructorReference(val function: Any) {
    override fun toString(): String = "ConstructorReference"
}

fun box(): String {
    val integers: (Int) -> Constructed = ::Constructed
    val sameIntegers: (Int) -> Constructed = ::Constructed
    val strings: (String) -> Constructed = ::Constructed
    val sameStrings: (String) -> Constructed = ::Constructed
    if (integers(7).value != 7 || strings("abc").value != 3) return "invoke"
    val intHolder = ConstructorReference(integers)
    val stringHolder = ConstructorReference(strings)
    if (intHolder != ConstructorReference(sameIntegers)) return "Int declaration equality"
    if (stringHolder != ConstructorReference(sameStrings)) return "String declaration equality"
    if (intHolder.hashCode() != ConstructorReference(sameIntegers).hashCode()) return "Int hash"
    if (stringHolder.hashCode() != ConstructorReference(sameStrings).hashCode()) return "String hash"
    if (intHolder == stringHolder) return "overloaded declarations"
    return "OK"
}
