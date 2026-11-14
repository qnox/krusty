// TARGET_BACKEND: JVM_IR
// PLATFORM-SPECIFIC: the imported owner is a JDK class whose static methods only the JVM has.
import java.lang.Math.*
import java.util.Collections.*

fun box(): String {
    val sorted = listOf(3, 1, 2).sorted()
    val reversed = sorted.toMutableList().also { reverse(it) }
    if (max(abs(-3), 2) != 3) return "Fail: Math"
    if (reversed != listOf(3, 2, 1)) return "Fail: Collections $reversed"
    return "OK"
}
