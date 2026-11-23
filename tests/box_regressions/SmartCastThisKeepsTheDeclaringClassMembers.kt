// After `check(this is Cancellable)`, `this` is both the declaring class and `Cancellable`; the
// class's own members, private ones included, stay in scope for bare and `this.`-qualified access.

interface Event

interface Cancellable : Event {
    fun reason(): String = "cancelled"
}

abstract class AbstractEvent : Event {
    private var cancelledFlag = false
    var other = 1

    val isCancelled: Boolean get() = cancelledFlag

    fun bump() {
        other = other + 1
    }

    fun cancel(): String {
        check(this is Cancellable) { "not cancellable" }
        cancelledFlag = true
        other = other + 1
        bump()
        return reason()
    }

    fun qualifiedCancel(): String {
        check(this is Cancellable) { "not cancellable" }
        this.cancelledFlag = true
        this.other = this.other + 1
        this.bump()
        return this.reason() + this.other + this.isCancelled
    }

    fun qualifiedAfterTest(): Int {
        if (this is Cancellable) return this.other + this.other
        return -1
    }

    fun otherAfterTest(): Int {
        if (this is Cancellable) return other + other
        return -1
    }
}

fun AbstractEvent.cancelFromExtension(): String {
    if (this is Cancellable) {
        bump()
        return reason() + other
    }
    return "plain"
}

class Plain : AbstractEvent()

class Stoppable : AbstractEvent(), Cancellable

fun box(): String {
    val stoppable = Stoppable()
    if (stoppable.cancel() != "cancelled") return "reason"
    if (!stoppable.isCancelled) return "not cancelled"
    if (stoppable.other != 3) return "other: ${stoppable.other}"
    val qualified = Stoppable()
    if (qualified.qualifiedCancel() != "cancelled3true") return "qualified: ${qualified.qualifiedCancel()}"
    if (Stoppable().qualifiedAfterTest() != 2) return "qualified after"
    if (Plain().qualifiedAfterTest() != -1) return "qualified plain"
    if (stoppable.otherAfterTest() != 6) return "after: ${stoppable.otherAfterTest()}"
    if (stoppable.cancelFromExtension() != "cancelled4") return "extension: ${stoppable.cancelFromExtension()}"
    if (Plain().cancelFromExtension() != "plain") return "plain extension"
    if (Plain().otherAfterTest() != -1) return "plain"
    try {
        Plain().cancel()
        return "plain cancelled"
    } catch (expected: IllegalStateException) {
        return "OK"
    }
}
