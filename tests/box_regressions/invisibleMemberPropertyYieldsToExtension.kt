// A member property the read cannot see does not end the read's scope tower: an extension
// property of the same name is a later level and wins. Inside the class the member is visible
// and keeps its precedence.
class Tagged {
    private val tag: String = "member"
    fun own(): String = tag
}

val Tagged.tag: String get() = "extension"

fun box(): String {
    val tagged = Tagged()
    val result = tagged.tag + " " + tagged.own()
    return if (result == "extension member") "OK" else result
}
