// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A library composable extension function: its receiver is the first tracked parameter, ahead of
// the declared ones, and `$default` numbers only the declared ones. The call passes the receiver,
// the caller's composer, `$changed`, and the mask.
// MODULE: lib
// FILE: lib.kt
package lib

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

// A composable lambda parameter after the receiver is still recognised as one.
@Composable
fun String.Wrap(content: @Composable () -> Unit) {
    content()
}

// MODULE: main(lib)
// FILE: greeting.kt
package probe

import androidx.compose.runtime.Composable
import lib.Label
import lib.Plain

@Composable
fun Greeting() {
    "a".Label()
    "b".Label(5)
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
        .getMethod("Greeting", Composer::class.java, Integer.TYPE)
        .invoke(null, composer, 0)
    return if (lib.seen.toString() == "a1;b5;7;") "OK" else log.toString()
}
