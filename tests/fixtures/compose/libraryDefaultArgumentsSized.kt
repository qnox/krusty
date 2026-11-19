// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A library composable whose only parameter has a default, called with that argument supplied: the
// call still passes the caller's composer, `$changed`, and the `$default` mask of no omitted
// parameter.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

@Composable
fun Sized(size: Int = 1) {}

// MODULE: main(lib)
// FILE: greeting.kt
package probe

import androidx.compose.runtime.Composable

@Composable
fun Greeting() {
    lib.Sized(2)
}

// FILE: box.kt
package probe

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy

fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

// A proxy composer records the calls the two transformed functions make on it.
fun box(): String {
    val log = StringBuilder()
    val scope = implement(ScopeUpdateScope::class.java) { _, _, _ -> null }
    val composer = implement(Composer::class.java) { proxy, method, args ->
        val name: String = method.name
        log.append(name)
        if (name == "changed") log.append(' ').append(args[0])
        log.append(';')
        when (name) {
            "startRestartGroup" -> proxy
            "endRestartGroup" -> scope
            "changed" -> true
            "getSkipping" -> false
            "shouldExecute" -> true
            else -> null
        }
    }
    Class.forName("probe.GreetingKt")
        .getMethod("Greeting", Composer::class.java, Integer.TYPE)
        .invoke(null, composer, 0)
    val expected = "startRestartGroup;sourceInformation;shouldExecute;startRestartGroup;sourceInformation;shouldExecute;endRestartGroup;endRestartGroup;"
    return if (log.toString() == expected) "OK" else log.toString()
}
