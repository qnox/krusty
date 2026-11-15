// A class extending an abstract library class: its constructor delegates to the library
// constructor, and it reads a member the library class declares through the subclass.
import kotlin.coroutines.AbstractCoroutineContextElement
import kotlin.coroutines.CoroutineContext

class Name(val name: String) : AbstractCoroutineContextElement(Name) {
    companion object Key : CoroutineContext.Key<Name>
}

fun box(): String {
    val element = Name("x")
    if (element.key !== Name) return "fail: key"
    if (element.name != "x") return "fail: ${element.name}"
    return "OK"
}
