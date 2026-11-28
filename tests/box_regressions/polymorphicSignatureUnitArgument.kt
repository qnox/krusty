// TARGET_BACKEND: JVM_IR
// FULL_JDK
// PLATFORM-SPECIFIC: `MethodHandle` and `VarHandle` calls are JVM signature-polymorphic invocations.
// WITH_STDLIB

import java.lang.invoke.MethodHandles
import java.lang.invoke.MethodType

var result: Any? = null

class C {
    fun foo(u: Unit) {
        result = u
    }
}

fun box(): String {
    val mh = MethodHandles.lookup().findVirtual(
        C::class.java, "foo",
        MethodType.methodType(Void.TYPE, Unit::class.java)
    )

    mh.invokeExact(C(), Unit)

    return if (result === Unit) "OK" else "Fail: $result"
}
