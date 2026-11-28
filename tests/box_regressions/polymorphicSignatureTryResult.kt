// TARGET_BACKEND: JVM_IR
// FULL_JDK
// PLATFORM-SPECIFIC: `MethodHandle` and `VarHandle` calls are JVM signature-polymorphic invocations.
// WITH_STDLIB

import java.lang.invoke.MethodHandles
import java.lang.invoke.MethodType

object O {
    var counter = 0
    fun main() {
        counter += 1
    }
}

fun box(): String {
    try {
        val mh = MethodHandles.lookup().findVirtual(O::class.java, "main", MethodType.methodType(Void.TYPE))
        mh.invokeExact(O)
        mh.invokeExact(O)
    } finally {}
    if (O.counter != 2) return "Fail: counter == ${O.counter}"
    return "OK"
}
