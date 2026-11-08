// A data class's generated `equals` and `hashCode` follow each property's own rules, and a
// program can print the hash, so every target must answer kotlinc's number.
data class Scalars(val a: Int, val b: Long, val c: Boolean, val d: Char, val e: Byte, val f: Short)

data class References(val s: String, val n: Int?, val l: Long?)

data class Nested(val inner: References, val b: Boolean?)

data class UnitHolder(val value: Unit)

fun box(): String {
    val scalars = Scalars(1, 2L, true, 'x', -3, -4)
    val expected = ((((1 * 31 + 2) * 31 + 1231) * 31 + 120) * 31 - 3) * 31 - 4
    if (scalars.hashCode() != expected) return "fail scalar hash ${scalars.hashCode()}"
    if (scalars != Scalars(1, 2L, true, 'x', -3, -4)) return "fail scalar equals"
    if (scalars == Scalars(1, 3L, true, 'x', -3, -4)) return "fail scalar not equals"

    // "ab" hashes to 97 * 31 + 98; `1L shl 40` folds to 256; null hashes to 0.
    val references = References("ab", null, 1L shl 40)
    if (references.hashCode() != 2984161) return "fail reference hash ${references.hashCode()}"
    if (References("ab", 7, null).hashCode() != 2984122) return "fail boxed hash"
    if (references != References("ab", null, 1L shl 40)) return "fail reference equals"
    if (references == References("ab", 1, 1L shl 40)) return "fail reference not equals"

    val nested = Nested(references, false)
    if (nested.hashCode() != 92510228) return "fail nested hash ${nested.hashCode()}"
    if (nested != Nested(References("ab", null, 1L shl 40), false)) return "fail nested equals"
    if (nested == Nested(references, true)) return "fail nested not equals"

    if (UnitHolder(Unit).hashCode() != UnitHolder(Unit).hashCode()) return "fail Unit hash"
    if (UnitHolder(Unit) != UnitHolder(Unit)) return "fail Unit equals"
    return "OK"
}
