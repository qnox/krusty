var receiverEqualityCalls = 0

class CountingReceiver {
    fun value(): Int = 7
    override fun equals(other: Any?): Boolean {
        receiverEqualityCalls += 1
        return false
    }
    override fun hashCode(): Int = 0
}

data class CountingReference(val function: () -> Int) {
    override fun toString(): String = "CountingReference"
}

fun box(): String {
    val receiver = CountingReceiver()
    val reference = receiver::value
    val first = CountingReference(reference)
    val shared = CountingReference(reference)
    if (first != shared) return "reference self equality"
    if (receiverEqualityCalls != 0) return "self equality called receiver"
    if (first == CountingReference(receiver::value)) return "distinct reference receiver equality"
    if (receiverEqualityCalls != 1) return "distinct reference receiver calls"
    return "OK"
}
