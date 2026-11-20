// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable function with a result and no composable call in its body has no group of its own:
// it takes the composer and `$changed`, and its source statements run between the trace markers,
// the result held across the closing one.
// FILE: resultFunctions.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Next(value: Int): Int {
    return value + 1
}

@Composable
fun Label(value: Int, prefix: String): String = prefix + value

@Composable
fun Both(left: Long, right: Long, flag: Boolean): Long {
    val sum = left + right
    return if (flag) sum else -sum
}

@Composable
fun Version(): Int = 7

@Composable
fun Same(value: Int): Int = value

// FILE: box.kt
package screens

import androidx.compose.runtime.Composer
import java.lang.reflect.Proxy

// A proxy composer records the source information markers that bracket each function.
fun box(): String {
    val log = StringBuilder()
    val composer = Composer::class.java.cast(
        Proxy.newProxyInstance(Composer::class.java.classLoader, arrayOf(Composer::class.java)) { _, method, _ ->
            val name: String = method.name
            when (name) {
                "sourceInformationMarkerStart" -> log.append("start;")
                "sourceInformationMarkerEnd" -> log.append("end;")
                else -> throw IllegalStateException(name)
            }
            null
        }
    )
    val facade = Class.forName("screens.ResultFunctionsKt")
    val int = Integer.TYPE
    val long = java.lang.Long.TYPE
    val next = facade.getMethod("Next", int, Composer::class.java, int).invoke(null, 4, composer, 0)
    if (next != 5) return "Next: $next"
    val label = facade.getMethod("Label", int, String::class.java, Composer::class.java, int)
        .invoke(null, 4, "n", composer, 0)
    if (label != "n4") return "Label: $label"
    val both = facade
        .getMethod("Both", long, long, java.lang.Boolean.TYPE, Composer::class.java, int)
        .invoke(null, 2L, 3L, false, composer, 0)
    if (both != -5L) return "Both: $both"
    val version = facade.getMethod("Version", Composer::class.java, int).invoke(null, composer, 0)
    if (version != 7) return "Version: $version"
    val same = facade.getMethod("Same", int, Composer::class.java, int).invoke(null, 9, composer, 0)
    if (same != 9) return "Same: $same"
    if (log.toString() != "start;end;start;end;start;end;start;end;start;end;") return log.toString()
    return "OK"
}
