// MODULE: library
// FILE: Library.kt
package signedarrays

data class SignedArrays(
    val bytes: ByteArray,
    val shorts: ShortArray,
    val ints: IntArray,
    val longs: LongArray,
)

fun makeSignedArrays(): SignedArrays = SignedArrays(
    byteArrayOf(-1, 2),
    shortArrayOf(-3, 4),
    intArrayOf(-5, 6),
    longArrayOf(-7, 8),
)

// MODULE: main(library)
// FILE: Box.kt
import signedarrays.makeSignedArrays

fun box(): String {
    val value = makeSignedArrays()
    if (value.toString() != "SignedArrays(bytes=[-1, 2], shorts=[-3, 4], ints=[-5, 6], longs=[-7, 8])") {
        return "data class array toString"
    }
    // These are the signed primitive arrays' content hashes, combined in data-class property order.
    if (value.hashCode() != 28629531) return "data class array hash: ${value.hashCode()}"
    return "OK"
}
