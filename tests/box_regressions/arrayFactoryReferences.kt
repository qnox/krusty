// A reference to a compiler array factory is an ordinary callable reference to that declaration:
// unadapted it takes and returns the whole array, adapted it builds the array from its values.
// An unsigned factory exchanges its `UIntArray`/`ULongArray` through `invoke` as the box.
@file:OptIn(ExperimentalUnsignedTypes::class)

fun <T, R> apply(f: (T) -> R, arg: T): R = f(arg)

fun box(): String {
    val ints = ::intArrayOf
    if (ints(intArrayOf(4))[0] != 4) return "fail int"
    val pair: (Int, Int) -> IntArray = ::intArrayOf
    val built = pair(4, 5)
    if (!built.contentEquals(intArrayOf(4, 5))) return "fail adapted int"
    val chars: (Char) -> CharArray = ::charArrayOf
    if (chars('k')[0] != 'k') return "fail adapted char"

    val uints = ::uintArrayOf
    if (uints(uintArrayOf(42u))[0] != 42u) return "fail uint"
    if (apply(::uintArrayOf, uintArrayOf(7u))[0] != 7u) return "fail generic uint"
    val uintPair: (UInt, UInt) -> UIntArray = ::uintArrayOf
    val builtUInts = uintPair(1u, 2u)
    if (!builtUInts.contentEquals(uintArrayOf(1u, 2u))) return "fail adapted uint"
    val ulongPair: (ULong, ULong) -> ULongArray = ::ulongArrayOf
    val builtULongs = ulongPair(1uL, 2uL)
    if (!builtULongs.contentEquals(ulongArrayOf(1uL, 2uL))) return "fail adapted ulong"
    val ubytes: (UByte) -> UByteArray = ::ubyteArrayOf
    if (ubytes(3u)[0] != 3.toUByte()) return "fail adapted ubyte"

    return "OK"
}
