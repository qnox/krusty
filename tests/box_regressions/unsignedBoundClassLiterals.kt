// `value::class` of an unsigned value names its own classifier, never the signed carrier the
// value sits in: a literal, a local, a parameter, and a narrow `UByte` compared with its literal.

fun ofParam(x: UInt) = x::class.simpleName
fun box(): String {
    val literal = 1u::class.simpleName
    val long = 3uL
    val local = long::class.simpleName
    val parameter = ofParam(4u)
    val byte: UByte = 1u
    val narrow = byte::class
    val same = narrow == UByte::class
    return if (literal == "UInt" && local == "ULong" && parameter == "UInt" && same) "OK"
    else "unsigned class literals: $literal $local $parameter ${narrow.simpleName}"
}
