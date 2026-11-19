// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// An extension receiver is a tracked value like a parameter: its stability is inferred from its type
// (a `List` is unstable), and a function with a result and no composable call has no group.
// FILE: greeting.kt
package probe

import androidx.compose.runtime.Composable

val seen = StringBuilder()

@Composable
fun List<String>.Count(label: String) {
    seen.append(size).append(label).append(';')
}

@Composable
fun String.Twice(): String = this + this

@Composable
fun Greeting(names: List<String>) {
    names.Count("x")
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
        .getMethod("Greeting", List::class.java, Composer::class.java, Integer.TYPE)
        .invoke(null, listOf("p", "q", "r"), composer, 0)
    return if (seen.toString() == "3x;") "OK" else log.toString()
}
