// MODULE: lib
// FILE: Slot.kt
package boundary

interface Slot<T> {
    fun exchange(value: T): T
}

// MODULE: main(lib)
// FILE: Implementations.kt
package boundary

OPTIONAL_JVM_INLINE_ANNOTATION
value class Ticket(val bits: Long)

var trace = ""

class IntSlot : Slot<Int> {
    var seen = 0
    override fun exchange(value: Int): Int {
        trace += "I"
        seen = value
        return value + 1
    }
}

class ErasedSlot<T> : Slot<T> {
    var seen: Any? = null
    override fun exchange(value: T): T {
        seen = value
        return value
    }
}

fun intReceiver(value: Slot<Int>): Slot<Int> { trace += "R"; return value }
fun intArgument(value: Int): Int { trace += "A"; return value }
fun observedLong(value: ErasedSlot<Long>): Any? = value.seen
fun observedTicket(value: ErasedSlot<Ticket>): Any? = value.seen
fun observedReference(value: ErasedSlot<String>): Any? = value.seen

// FILE: Main.kt
package boundary

fun box(): String {
    val ints = IntSlot()
    val intSlot: Slot<Int> = ints
    trace = ""
    val intResult = intReceiver(intSlot).exchange(intArgument(7))
    if (intResult != 8 || ints.seen != 7 || trace != "RAI") return "fail Int boundary"

    val bits = 4294967305L
    val longs = ErasedSlot<Long>()
    val longSlot: Slot<Long> = longs
    val longResult = longSlot.exchange(bits)
    if (longResult != bits) return "fail Long result"
    val longObject = observedLong(longs)
    val longIsTicket = longObject is Ticket
    if (longObject !is Long || longIsTicket) return "fail Long box identity"
    if (longObject != bits) return "fail Long argument"

    val tickets = ErasedSlot<Ticket>()
    val ticketSlot: Slot<Ticket> = tickets
    val ticketResult = ticketSlot.exchange(Ticket(bits))
    if (ticketResult.bits != bits) return "fail value class result"
    val ticketObject = observedTicket(tickets)
    val ticketIsLong = ticketObject is Long
    if (ticketObject !is Ticket || ticketIsLong) return "fail value class box identity"
    if (ticketObject.bits != bits) return "fail value class argument"
    val references = ErasedSlot<String>()
    val referenceSlot: Slot<String> = references
    val word = "OK"
    val referenceResult = referenceSlot.exchange(word)
    if (referenceResult !== word || observedReference(references) !== word) return "fail unchanged reference boundary"
    return "OK"
}
