open class ReceiverParent
class ReceiverChild : ReceiverParent()

fun ReceiverParent.ownRead(): ReceiverChild? = this as? ReceiverChild

fun ReceiverParent.capturedRead(): ReceiverChild? {
    fun read(): ReceiverChild? = this as? ReceiverChild
    return read()
}

fun ReceiverParent.lambdaRead(): ReceiverChild? {
    val read = { this as? ReceiverChild }
    return read()
}

inline fun ReceiverParent.readInline(block: ReceiverParent.() -> ReceiverChild?): ReceiverChild? = block()
fun ReceiverParent.inlinedRead(): ReceiverChild? = readInline { this as? ReceiverChild }

var receiverCalls = 0
fun nextReceiver(): ReceiverParent {
    receiverCalls += 1
    return ReceiverChild()
}

fun computedRead(): ReceiverChild? = nextReceiver() as? ReceiverChild

fun sharedCellRead(): ReceiverChild? {
    var current: ReceiverParent = ReceiverChild()
    val read = { current as? ReceiverChild }
    current = ReceiverParent()
    return read()
}

fun box(): String {
    val child = ReceiverChild()
    if (child.ownRead() !== child) return "own"
    if (child.capturedRead() !== child) return "capture"
    if (child.lambdaRead() !== child) return "lambda"
    if (child.inlinedRead() !== child) return "inline"
    val parent = ReceiverParent()
    if (parent.ownRead() != null || parent.capturedRead() != null) return "parent"
    if (parent.lambdaRead() != null || parent.inlinedRead() != null) return "parent lambda"
    if (computedRead() == null || receiverCalls != 1) return "computed"
    if (sharedCellRead() != null) return "cell"
    return "OK"
}
