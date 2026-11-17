// Interface defaults a library interface declares, called on source classes that inherit them
// without overriding: directly, and through a library superclass that inherits them itself.
import kotlin.coroutines.AbstractCoroutineContextElement
import kotlin.coroutines.CoroutineContext
import kotlin.coroutines.EmptyCoroutineContext

class Name(val name: String) : AbstractCoroutineContextElement(Name) {
    companion object Key : CoroutineContext.Key<Name>
}

class Direct : CoroutineContext.Element {
    override val key: CoroutineContext.Key<*> get() = Direct
    companion object Key : CoroutineContext.Key<Direct>
}

object OtherKey : CoroutineContext.Key<Name>

fun box(): String {
    val element = Name("x")
    if (element.minusKey(Name) !== EmptyCoroutineContext) return "fail: minusKey"
    if (element.minusKey(OtherKey) !== element) return "fail: minusKey other"
    if (element[Name] !== element) return "fail: get"
    if (element[OtherKey] != null) return "fail: get other"
    if (element.fold("") { acc, e -> acc + (e as Name).name } != "x") return "fail: fold"
    val direct = Direct()
    if (direct.minusKey(Direct) !== EmptyCoroutineContext) return "fail: direct minusKey"
    if (direct[Direct] !== direct) return "fail: direct get"
    return "OK"
}
