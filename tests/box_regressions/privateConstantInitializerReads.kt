// A singleton `const val` initializer may read a private constant that is accessible where the
// initializer is written. Its value is a compile-time constant that every read inlines.

object Bits {
    private const val WIDTH = 3
    private const val MASK = (1 shl WIDTH) - 1
    const val PUBLIC_MASK = MASK shl 1

    fun mask() = MASK

    object Nested {
        const val DOUBLE = WIDTH * 2
    }
}

class Version(val major: Int, val minor: Int) {
    fun encode(): Int = major or (minor shl MAJOR_BITS)

    companion object {
        private const val MAJOR_BITS = 3
        private const val MAJOR_MASK = (1 shl MAJOR_BITS) - 1

        fun decode(version: Int): Version =
            Version(version and MAJOR_MASK, version shr MAJOR_BITS)
    }
}

class Holder {
    private object Limits {
        const val MAX = 40
    }

    companion object {
        const val LIMIT = Limits.MAX + 2
    }
}

fun box(): String {
    if (Bits.mask() != 7) return "fail mask ${Bits.mask()}"
    if (Bits.PUBLIC_MASK != 14) return "fail public mask ${Bits.PUBLIC_MASK}"
    if (Bits.Nested.DOUBLE != 6) return "fail nested ${Bits.Nested.DOUBLE}"
    val decoded = Version.decode(Version(5, 2).encode())
    if (decoded.major != 5 || decoded.minor != 2) return "fail version ${decoded.major}.${decoded.minor}"
    if (Holder.LIMIT != 42) return "fail limit ${Holder.LIMIT}"
    return "OK"
}
