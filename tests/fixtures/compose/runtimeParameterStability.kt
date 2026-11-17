// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable function parameter of an interface, or of a class of another module compiled with
// Compose, has a stability only known when the function runs.
// MODULE: lib
// FILE: lib.kt
package lib

class Point(val x: Int, val y: Int)

interface Shape

// MODULE: main(lib)
// FILE: screens.kt
package screens

import androidx.compose.runtime.Composable
import lib.Point
import lib.Shape

@Composable
fun ShowPoint(p: Point) {
    println(p.x)
}

@Composable
fun ShowShape(s: Shape) {
    println(s)
}

@Composable
fun Screen(p: Point, s: Shape) {
    ShowPoint(p)
    ShowShape(s)
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
    Class.forName("screens.ScreensKt").getMethod(
        "Screen",
        lib.Point::class.java,
        lib.Shape::class.java,
        Composer::class.java,
        Integer.TYPE,
    ).invoke(null, lib.Point(3, 4), object : lib.Shape {}, composer, 0)
    val expected = "changed;changed;execute true;execute true;execute true;"
    return if (log.toString() == expected) "OK" else log.toString()
}
