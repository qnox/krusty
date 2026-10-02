// Kotlin's answers for `unsigned_unbox_null.c`: unboxing a null unsigned is a catchable
// NullPointerException.
fun nullUByte(): UByte? = null
fun nullUShort(): UShort? = null
fun nullUInt(): UInt? = null
fun nullULong(): ULong? = null

fun box(): String {
    val out = StringBuilder()
    fun unboxed(label: String, block: () -> Any) {
        val answer = try {
            block().toString()
        } catch (e: NullPointerException) {
            "${e::class.simpleName}: ${e.message}"
        }
        out.append("$label = $answer\n")
    }
    unboxed("UByte?!!") { nullUByte()!! }
    unboxed("UShort?!!") { nullUShort()!! }
    unboxed("UInt?!!") { nullUInt()!! }
    unboxed("ULong?!!") { nullULong()!! }
    return out.toString()
}
