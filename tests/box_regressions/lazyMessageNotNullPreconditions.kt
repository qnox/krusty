// WITH_STDLIB
// The stdlib's lazy-message `checkNotNull` and `requireNotNull` return their `value: T?` as `T`
// after the `null` test, which their erased inline bodies leave without a cast.
fun present(value: String?): String = checkNotNull(value) { "absent" }

fun required(value: Int?): Int = requireNotNull(value) { "required" }

fun box(): String {
    if (present("text") != "text") return "checkNotNull"
    if (required(7) + 1 != 8) return "requireNotNull"
    val checked = try {
        present(null)
        "no failure"
    } catch (failure: IllegalStateException) {
        failure.message
    }
    if (checked != "absent") return "checkNotNull message: $checked"
    val rejected = try {
        required(null)
        "no failure"
    } catch (failure: IllegalArgumentException) {
        failure.message
    }
    if (rejected != "required") return "requireNotNull message: $rejected"
    return "OK"
}
