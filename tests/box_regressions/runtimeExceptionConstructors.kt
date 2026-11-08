private var exceptionMessageCalls = 0

fun nextExceptionMessage(): String {
    exceptionMessageCalls += 1
    return "retained message"
}

fun catchSameException(original: Throwable): Throwable {
    return try {
        throw original
    } catch (caught: Throwable) {
        caught
    }
}

fun box(): String {
    exceptionMessageCalls = 0
    val original = IllegalStateException(nextExceptionMessage())
    val caught = catchSameException(original)
    if (caught !== original) return "replaced exception"
    if (caught !is IllegalStateException) return "lost selected subtype"
    if (caught !is RuntimeException || caught !is Exception) return "lost parent type"
    if (caught is IndexOutOfBoundsException) return "wrong sibling type"
    if (caught.message != "retained message") return "lost message"
    if (exceptionMessageCalls != 1) return "message evaluated again"

    val empty = IllegalStateException()
    val absent: String? = null
    val nullMessage = IllegalStateException(absent)
    if (empty === nullMessage) return "shared exception"
    if (catchSameException(empty) !== empty) return "replaced empty exception"
    if (empty.message != null || nullMessage.message != null) return "invented message"
    if (exceptionMessageCalls != 1) return "unexpected constructor effect"
    return "OK"
}
