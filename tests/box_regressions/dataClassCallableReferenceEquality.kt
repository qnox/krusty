class Receiver(val value: Int) {
    fun apply(argument: Int): Int = value + argument
    override fun equals(other: Any?): Boolean = other is Receiver && other.value == value
    override fun hashCode(): Int = value
    override fun toString(): String = "Receiver"
}

fun increment(value: Int): Int = value + 1
fun decrement(value: Int): Int = value - 1

var receiverCalls = 0
fun makeReceiver(): Receiver {
    receiverCalls += 1
    return Receiver(7)
}

data class BoundReference(val function: (Int) -> Int) {
    override fun toString(): String = "BoundReference"
}

data class UnboundReference(val function: (Receiver, Int) -> Int) {
    override fun toString(): String = "UnboundReference"
}

fun box(): String {
    val first = Receiver(7)
    val equalReceiver = Receiver(7)
    val bound = BoundReference(first::apply)
    val equivalentBound = BoundReference(equalReceiver::apply)
    if (bound.function(2) != 9) return "bound invoke"
    if (bound != equivalentBound) return "bound equality"
    if (bound.hashCode() != equivalentBound.hashCode()) return "bound hash"
    if (bound == BoundReference(Receiver(8)::apply)) return "unequal receiver"
    val evaluated = BoundReference(makeReceiver()::apply)
    if (receiverCalls != 1) return "receiver evaluation count"
    if (evaluated.function(4) != 11) return "evaluated receiver invoke"
    if (evaluated != bound) return "evaluated receiver equality"
    if (receiverCalls != 1) return "receiver reevaluation"

    val unbound = UnboundReference(Receiver::apply)
    val equivalentUnbound = UnboundReference(Receiver::apply)
    if (unbound.function(first, 3) != 10) return "unbound invoke"
    if (unbound != equivalentUnbound) return "unbound equality"
    if (unbound.hashCode() != equivalentUnbound.hashCode()) return "unbound hash"

    val topLevel = BoundReference(::increment)
    val equivalentTopLevel = BoundReference(::increment)
    if (topLevel != equivalentTopLevel) return "top-level equality"
    if (topLevel.hashCode() != equivalentTopLevel.hashCode()) return "top-level hash"
    if (topLevel == BoundReference(::decrement)) return "distinct declarations"

    val captured = 3
    fun local(value: Int): Int = captured + value
    val firstLocal = BoundReference(::local)
    val secondLocal = BoundReference(::local)
    if (firstLocal.function(4) != 7) return "local invoke"
    if (firstLocal != secondLocal) return "local declaration equality"
    if (firstLocal.hashCode() != secondLocal.hashCode()) return "local declaration hash"
    return "OK"
}
