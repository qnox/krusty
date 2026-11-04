// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// Parameterless composable functions become restartable, skippable groups with source
// information, trace markers and a durable key, and pass their composer to each other.
// FILE: restartableFunctions.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Leaf() {
}

@Composable
fun Screen() {
    Leaf()
    Leaf()
}

@Composable
fun Labelled() {
    val label = "screen"
    println(label)
    Screen()
}

// FILE: box.kt
package screens

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy

// The runtime's own composer needs a composition this pass cannot build yet; a proxy composer
// records the group calls the transformed functions make, and keeps every recompose scope.
fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

fun box(): String {
    val log = StringBuilder()
    var execute = true
    val scopes = ArrayList<(Composer, Int) -> Unit>()
    val scope = implement(ScopeUpdateScope::class.java) { _, _, args ->
        @Suppress("UNCHECKED_CAST")
        scopes.add(args[0] as (Composer, Int) -> Unit)
        null
    }
    val composer = implement(Composer::class.java) { proxy, method, _ ->
        val name: String = method.name
        when (name) {
            "startRestartGroup" -> {
                log.append("start;")
                proxy
            }
            "sourceInformation" -> null
            "shouldExecute" -> execute
            "skipToGroupEnd" -> {
                log.append("skip;")
                null
            }
            "endRestartGroup" -> {
                log.append("end;")
                scope
            }
            else -> throw IllegalStateException(name)
        }
    }
    val labelled = Class.forName("screens.RestartableFunctionsKt")
        .getMethod("Labelled", Composer::class.java, Integer.TYPE)
    labelled.invoke(null, composer, 0)
    log.append(scopes.size)
    if (log.toString() != "start;start;start;end;start;end;end;end;4") return log.toString()
    log.setLength(0)
    execute = false
    // Recompose `Screen` with nothing changed: its group is skipped.
    scopes[2](composer, 0)
    log.append(scopes.size)
    if (log.toString() != "start;skip;end;5") return log.toString()
    return "OK"
}
