// TARGET_BACKEND: JVM_IR
// FULL_JDK
// PLATFORM-SPECIFIC: `MethodHandle` and `VarHandle` calls are JVM signature-polymorphic invocations.
// WITH_STDLIB
import java.lang.invoke.MethodHandles
import java.lang.invoke.MethodType

class Counter(@JvmField var value: Int)

fun box(): String {
    val lookup = MethodHandles.lookup()
    val handle = lookup.findVarHandle(Counter::class.java, "value", Int::class.java)
    val counter = Counter(40)
    if (!(handle.compareAndSet(counter, 40, 41) as Boolean)) return "Fail compareAndSet"
    val read: Int = handle.get(counter) as Int
    if (read != 41) return "Fail read: $read"
    val boxed = handle.get(counter) as Int?
    if (boxed != 41) return "Fail boxed: $boxed"
    val plain = handle.get(counter)
    if (plain != 41) return "Fail plain: $plain"
    val max = lookup.findStatic(Math::class.java, "max", MethodType.methodType(Int::class.java, Int::class.java, Int::class.java))
    val larger = max.invokeExact(3, 7) as Int
    if (larger != 7) return "Fail max: $larger"
    return "OK"
}
