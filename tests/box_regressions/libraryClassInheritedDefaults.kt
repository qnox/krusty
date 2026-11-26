// Interface defaults a library interface declares, called on source classes that inherit them
// without overriding: directly, and through a library superclass that inherits them itself. A
// class that overrides the same defaults, called through the interface beside them, keeps its
// overrides: an ordinary call dispatches on the receiver.
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

class Overriding : CoroutineContext.Element {
    override val key: CoroutineContext.Key<*> get() = Overriding
    companion object Key : CoroutineContext.Key<Overriding>

    @Suppress("UNCHECKED_CAST")
    override fun <E : CoroutineContext.Element> get(key: CoroutineContext.Key<E>): E? =
        if (key === OverridingKey) this as E else null

    override fun minusKey(key: CoroutineContext.Key<*>): CoroutineContext = Direct()

    override fun <R> fold(initial: R, operation: (R, CoroutineContext.Element) -> R): R = initial
}

object OtherKey : CoroutineContext.Key<Name>

object OverridingKey : CoroutineContext.Key<Overriding>

fun element(index: Int): CoroutineContext.Element = if (index == 0) Direct() else Overriding()

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
    val inherited = element(0)
    val overriding = element(1)
    if (inherited.minusKey(Direct) !== EmptyCoroutineContext) return "fail: inherited minusKey"
    if (inherited[Direct] !== inherited) return "fail: inherited get"
    if (inherited.fold(0) { acc, _ -> acc + 1 } != 1) return "fail: inherited fold"
    if (overriding.minusKey(Overriding) !is Direct) return "fail: overriding minusKey"
    if (overriding[Overriding] != null) return "fail: overriding get"
    if (overriding[OverridingKey] !== overriding) return "fail: overriding get other"
    if (overriding.fold(0) { acc, _ -> acc + 1 } != 0) return "fail: overriding fold"
    return "OK"
}
