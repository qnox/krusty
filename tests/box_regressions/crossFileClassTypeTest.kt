// FILE: Declarations.kt
open class Plain(val bits: Long)

class Derived(bits: Long) : Plain(bits)

class Other

OPTIONAL_JVM_INLINE_ANNOTATION
value class Ticket(val bits: Long)

// FILE: Main.kt
fun box(): String {
    val plain: Any = Plain(5L)
    if (plain !is Plain) return "fail Plain is Plain"
    if (plain is Derived) return "fail Plain is Derived"
    if (plain is Other) return "fail Plain is Other"
    val derived: Any = Derived(6L)
    if (derived !is Plain) return "fail Derived is Plain"
    if (derived !is Derived) return "fail Derived is Derived"
    val absent: Any? = null
    if (absent is Plain) return "fail null is Plain"
    if (absent !is Plain?) return "fail null is Plain?"
    val text: Any = "text"
    if (text is Plain) return "fail String is Plain"
    val ticket: Any = Ticket(7L)
    if (ticket !is Ticket) return "fail Ticket box is Ticket"
    if (ticket is Long) return "fail Ticket box is Long"
    val number: Any = 7L
    if (number is Ticket) return "fail Long is Ticket"
    return "OK"
}
