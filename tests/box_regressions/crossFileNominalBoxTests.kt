// FILE: Checks.kt
package nominal

fun isTicket(value: Any?): Boolean = value is Ticket
fun isOther(value: Any?): Boolean = value is Other
fun ticketOrNull(value: Any?): Ticket? = value as? Ticket
fun ticket(value: Any?): Ticket = value as Ticket
fun nullableTicket(value: Any?): Ticket? = value as Ticket?
fun isBase(value: Any?): Boolean = value is Base
fun longValue(value: Any?): Long = value as Long
fun nullableLong(value: Any?): Long? = value as Long?

// FILE: Definitions.kt
package nominal

OPTIONAL_JVM_INLINE_ANNOTATION
value class Ticket(val bits: Long)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Other(val bits: Long)
open class Base
class Child : Base()

// FILE: Main.kt
package nominal

fun box(): String {
    val bits = 4294967305L
    val boxed: Any = Ticket(bits)
    val other: Any = Other(bits)
    val scalar: Any = bits
    if (!isTicket(boxed) || isOther(boxed) || boxed is Long) return "fail Ticket identity"
    if (isTicket(other) || !isOther(other) || other is Long) return "fail Other identity"
    if (isTicket(scalar) || isOther(scalar) || scalar !is Long) return "fail Long identity"
    if (isTicket(null) || isTicket("OK")) return "fail unrelated identity"
    if (longValue(scalar) != bits || nullableLong(scalar) != bits) return "fail scalar casts"
    if (nullableLong(null) != null) return "fail nullable scalar cast"
    if (ticketOrNull(boxed)?.bits != bits || ticket(boxed).bits != bits) return "fail casts"
    if (ticketOrNull(other) != null || ticketOrNull(scalar) != null) return "fail safe casts"
    if (ticketOrNull(null) != null || nullableTicket(null) != null) return "fail nullable casts"
    if (!isBase(Child()) || isBase(boxed) || isBase(null)) return "fail class hierarchy"
    return "OK"
}
