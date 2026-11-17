// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable function parameter of a class type takes the stability kotlinc infers for the class:
// a stable class is compared by value, an unstable one by identity, and a call passes on what it
// knows of each argument.
// FILE: inferredStability.kt
package screens

import androidx.compose.runtime.Composable

class Point(val x: Int, val y: Int)

class Counter(var n: Int)

data class Named(val name: String, val point: Point)

@Composable
fun ShowNamed(named: Named?, pair: Pair<Int, String>) {
    println(named)
}

@Composable
fun ShowPoint(p: Point) {
    println(p.x)
}

@Composable
fun ShowCounter(c: Counter) {
    println(c.n)
}

@Composable
fun Screen(p: Point, c: Counter, named: Named?, pair: Pair<Int, String>) {
    ShowPoint(p)
    ShowCounter(c)
    ShowNamed(named, pair)
}

// FILE: box.kt
package screens

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy

fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

// A proxy composer records every comparison and skipping decision the transformed functions make.
fun box(): String {
    val log = StringBuilder()
    val scope = implement(ScopeUpdateScope::class.java) { _, _, _ -> null }
    val composer = implement(Composer::class.java) { proxy, method, args ->
        val name: String = method.name
        when (name) {
            "startRestartGroup" -> proxy
            "sourceInformation" -> null
            "changed" -> {
                log.append("changed;")
                true
            }
            "changedInstance" -> {
                log.append("changedInstance;")
                true
            }
            "shouldExecute" -> {
                log.append("execute ").append(args[0]).append(';')
                true
            }
            "skipToGroupEnd" -> null
            "endRestartGroup" -> scope
            else -> throw IllegalStateException(name)
        }
    }
    Class.forName("screens.InferredStabilityKt").getMethod(
        "Screen",
        Point::class.java,
        Counter::class.java,
        Named::class.java,
        Pair::class.java,
        Composer::class.java,
        Integer.TYPE,
    ).invoke(null, Point(3, 4), Counter(5), Named("n", Point(1, 2)), Pair(1, "a"), composer, 0)
    val expected = "changed;changedInstance;changed;changed;execute true;execute true;execute true;execute true;"
    return if (log.toString() == expected) "OK" else log.toString()
}
