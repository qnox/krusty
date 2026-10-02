// Kotlin's answers for `null_receiver_raises.c`: a null where a receiver belongs is a catchable
// NullPointerException with no message.
fun nullString(): String? = null

fun box(): String {
    return try {
        nullString()!!.length.toString() + "\n"
    } catch (e: NullPointerException) {
        "${e::class.simpleName}: ${e.message} ${(e as Any) is RuntimeException}\n"
    }
}
