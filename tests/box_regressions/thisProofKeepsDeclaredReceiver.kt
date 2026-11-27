// After `this is B`, `this` has the declared class and `B`: members of both resolve.
interface Cancellable {
    fun reason(): String = "cancellable"
}

abstract class Event {
    private var cancelledFlag = false
    var label = "none"

    val isCancelled: Boolean get() = cancelledFlag

    fun cancel() {
        check(this is Cancellable) { "not cancellable" }
        cancelledFlag = true
    }

    fun tag(): String {
        if (this is Cancellable) {
            label = "cancellable"
            return label + ":" + reason()
        }
        return label
    }
}

class Plain : Event()
class Stoppable : Event(), Cancellable

fun box(): String {
    val stoppable = Stoppable()
    stoppable.cancel()
    if (!stoppable.isCancelled) return "cancel did not apply"
    if (stoppable.tag() != "cancellable:cancellable") return "tag: ${stoppable.tag()}"
    val plain = Plain()
    if (plain.tag() != "none") return "plain tag: ${plain.tag()}"
    try {
        plain.cancel()
        return "plain cancel did not throw"
    } catch (_: IllegalStateException) {
    }
    return "OK"
}
