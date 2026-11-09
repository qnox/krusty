// A value class implementing the interface of its own underlying value is boxed where the program
// reads it as that interface, so the value keeps its class identity.
interface I {
    fun ok(): String
}

OPTIONAL_JVM_INLINE_ANNOTATION
value class IC(val i: I) : I by i

fun isValue(value: I): Boolean = value is IC

fun asInterface(ic: IC): I = ic

fun box(): String {
    val i = object : I {
        override fun ok(): String = "OK"
    }
    val local: I = IC(i)
    if (local !is IC) return "fail local"
    if (!isValue(IC(i))) return "fail argument"
    if (asInterface(IC(i)) !is IC) return "fail return"
    if (isValue(i)) return "fail underlying"
    return local.ok()
}
