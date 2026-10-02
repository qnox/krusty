// Kotlin's answers for `throwable_subclass_size.c`: a subclass of Throwable keeps its own fields
// beside the message.
class Tagged(message: String, val first: Long, val second: Long) : Exception(message)

fun box(): String {
    val tagged = Tagged("m", 0x1111111111111111L, 0x2222222222222222L)
    return "${tagged.message} ${tagged.first} ${tagged.second}\n"
}
