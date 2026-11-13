OPTIONAL_JVM_INLINE_ANNOTATION
value class Meter(val v: Int)
fun box(): String {
val u = arrayOfNulls<UInt>(2)
u[0] = 5u
val l = arrayOfNulls<ULong>(1)
l[0] = 2uL
val m = arrayOfNulls<Meter>(2)
m[1] = Meter(3)
val n = arrayOfNulls<Unit>(1)
n[0] = Unit
val ok = u[0] == 5u && u[1] == null && l[0] == 2uL && m[1]?.v == 3 && n[0] == Unit
return if (ok) "OK" else "fail"
}
