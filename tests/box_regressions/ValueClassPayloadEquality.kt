// WITH_STDLIB
interface Payload { val id: Int }
class Part(override val id: Int) : Payload {
    override fun equals(other: Any?): Boolean = other is Part && id == other.id
    override fun hashCode(): Int = id
}
OPTIONAL_JVM_INLINE_ANNOTATION
value class Wrapped(val payload: Payload)
fun box(): String {
    val left: Payload = Part(7)
    val right: Payload = Part(7)
    if (left != right) return "payload equality"
    val first: Any = Wrapped(left)
    val second: Any = Wrapped(right)
    return if (first == second) "OK" else "value equality used payload identity"
}
