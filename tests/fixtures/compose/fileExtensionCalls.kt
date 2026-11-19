// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A call to an extension composable of the same file: the receiver takes the first `$changed` slot,
// and the mask numbers only the declared parameters after it.
// FILE: greeting.kt
package probe

import androidx.compose.runtime.Composable

val seen = StringBuilder()

@Composable
fun String.Label(size: Int = 1) {
    seen.append(this).append(size).append(';')
}

// An extension that never reads its receiver tracks no comparison of it.
@Composable
fun String.Plain(size: Int) {
    seen.append(size).append(';')
}

@Composable
fun Greeting(name: String) {
    "a".Label()
    "b".Label(5)
    name.Label(2)
    "c".Plain(7)
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
        .getMethod("Greeting", String::class.java, Composer::class.java, Integer.TYPE)
        .invoke(null, "x", composer, 0)
    return if (seen.toString() == "a1;b5;x2;7;") "OK" else log.toString()
}
