// Shape tie-break, shared by calls and constructor delegation: a fixed arity beats a vararg
// even when the fixed declaration omits a default, and fewer omitted defaults win only when
// the vararg status matches. Numeric specificity is a parameter-type relation, so an integer
// literal selects Long over ULong, and Nothing selects the same pairs.

fun fixedBeatsVararg(x: Int, y: Int = 2) = "fixed"

fun fixedBeatsVararg(vararg xs: Int) = "vararg"

fun fewerDefaults(x: Int = 1, y: Int = 2) = "two"

fun fewerDefaults(x: Int = 1) = "one"

fun literalSigned(x: Long) = "long"

fun literalSigned(x: ULong) = "ulong"

fun nothingSigned(x: Long): String = "long"

fun nothingSigned(x: ULong): Int = 0

fun nothingSelectsLong(): String = nothingSigned(null!!)

fun nothingUnsigned(x: UInt): String = "uint"

fun nothingUnsigned(x: UByte): Int = 0

fun nothingSelectsUInt(): String = nothingUnsigned(null!!)

fun nothingSmallUnsigned(x: UShort): String = "ushort"

fun nothingSmallUnsigned(x: UByte): Int = 0

fun nothingSelectsUShort(): String = nothingSmallUnsigned(null!!)

fun nothingFloat(x: Double): String = "double"

fun nothingFloat(x: Float): Int = 0

fun nothingSelectsDouble(): String = nothingFloat(null!!)

fun nothingNullable(x: Int?): String = "int"

fun nothingNullable(x: Long?): Int = 0

fun nothingNullableSelectsInt(): String {
    val n: Nothing? = null
    return nothingNullable(n)
}

class FixedDelegation(val tag: String) {
    constructor(x: Int, y: Int = 2) : this("fixed")

    constructor(vararg xs: Int) : this("vararg")

    constructor(seed: Long) : this(1)
}

class FewerDelegation(val tag: String) {
    constructor(x: Int = 1, y: Int = 2) : this("two")

    constructor(x: Int = 1) : this("one")

    constructor(seed: Long) : this()
}

fun box(): String {
    if (fixedBeatsVararg(1) != "fixed") return "call fixed: ${fixedBeatsVararg(1)}"
    if (fewerDefaults() != "one") return "call fewer: ${fewerDefaults()}"
    if (literalSigned(1) != "long") return "literal: ${literalSigned(1)}"
    if (FixedDelegation(1L).tag != "fixed") return "ctor fixed: ${FixedDelegation(1L).tag}"
    if (FewerDelegation(1L).tag != "one") return "ctor fewer: ${FewerDelegation(1L).tag}"
    return "OK"
}
