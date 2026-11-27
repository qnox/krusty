var topLevel = "t"

class Holder(val tag: String)

fun box(): String {
    var mirrored by ::topLevel
    mirrored = "m"
    val holder = Holder("h")
    val own by holder::tag
    val result = topLevel + mirrored + own
    return if (result == "mmh") "OK" else result
}
