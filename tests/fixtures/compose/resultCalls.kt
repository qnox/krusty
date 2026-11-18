// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A restartable function calls composable functions with a result inside its expressions: the
// operands of a call and an operator, a variable's initializer, a string template. Each call
// passes the composer and what the caller knows of its arguments, like a call used as a statement.
// FILE: resultCalls.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Next(value: Int): Int {
    return value + 1
}

@Composable
fun Label(value: Int): String = "n$value"

@Composable
fun Screen(value: Int) {
    val next = Next(value)
    println(next + Label(next).length)
    println("${Next(3)} ${Label(value)}")
}

// FILE: box.kt
package screens

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy

fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

// A proxy composer records the groups and markers the transformed functions make.
fun box(): String {
    val log = StringBuilder()
    val scope = implement(ScopeUpdateScope::class.java) { _, _, _ -> null }
    val composer = implement(Composer::class.java) { proxy, method, args ->
        val name: String = method.name
        when (name) {
            "startRestartGroup" -> {
                log.append("group;")
                proxy
            }
            "endRestartGroup" -> scope
            "sourceInformation" -> null
            "sourceInformationMarkerStart" -> log.append("start;")
            "sourceInformationMarkerEnd" -> log.append("end;")
            "changed" -> true
            "shouldExecute" -> true
            else -> throw IllegalStateException(name)
        }
    }
    Class.forName("screens.ResultCallsKt")
        .getMethod("Screen", Integer.TYPE, Composer::class.java, Integer.TYPE)
        .invoke(null, 4, composer, 0)
    val expected = "group;start;end;start;end;start;end;start;end;"
    return if (log.toString() == expected) "OK" else log.toString()
}
