// A reference to an unsigned vararg function takes the whole unsigned array as its one
// parameter. `FunctionN.invoke` receives that array as its box (`kotlin/UIntArray`) and unboxes
// it to the carrier (`int[]`) the selected function takes, whether the reference is invoked
// directly, through a generic call, or after a round trip through `Any`.
@file:OptIn(ExperimentalUnsignedTypes::class)

fun uints(vararg xs: UInt): UInt = xs.sum()
fun ulongs(vararg xs: ULong): ULong = xs.sum()
fun ubytes(vararg xs: UByte): UInt = xs.sum()
fun ushorts(vararg xs: UShort): UInt = xs.sum()

fun <T, R> apply(f: (T) -> R, arg: T): R = f(arg)

fun box(): String {
    if ((::uints)(uintArrayOf(1u, 2u)) != 3u) return "fail direct uint"
    if ((::ulongs)(ulongArrayOf(1uL, 2uL)) != 3uL) return "fail direct ulong"
    if ((::ubytes)(ubyteArrayOf(1u, 2u)) != 3u) return "fail direct ubyte"
    if ((::ushorts)(ushortArrayOf(1u, 2u)) != 3u) return "fail direct ushort"

    if (apply(::uints, uintArrayOf(1u, 2u)) != 3u) return "fail generic uint"
    if (apply(::ulongs, ulongArrayOf(1uL, 2uL)) != 3uL) return "fail generic ulong"
    if (apply(::ubytes, ubyteArrayOf(1u, 2u)) != 3u) return "fail generic ubyte"

    val ref: (UIntArray) -> UInt = ::uints
    if (ref(uintArrayOf(4u, 5u)) != 9u) return "fail typed"
    val asAny: Any = ref
    @Suppress("UNCHECKED_CAST")
    if ((asAny as (UIntArray) -> UInt)(uintArrayOf(4u, 5u)) != 9u) return "fail through Any"

    return "OK"
}
