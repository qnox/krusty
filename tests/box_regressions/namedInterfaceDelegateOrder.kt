interface Left { fun left(): String }
interface Right { fun right(): String }
class LeftValue(val text: String) : Left { override fun left() = text }
class RightValue(val text: String) : Right { override fun right() = text }
var events = ""
fun mark(text: String): String { events += text; return text }
open class Parent(text: String) { init { events += "B" } }
class Delegating(value: String) : Parent(mark("S")),
    Left by LeftValue(mark("L") + value),
    Right by RightValue(mark("R") + value) {
    val initialized = mark("P")
    init { events += "I" }
}
fun box(): String {
    events = ""
    val result = Delegating("K")
    val observed = events + ":" + result.left() + result.right()
    return if (observed == "SBLRPI:LKRK") "OK" else observed
}
