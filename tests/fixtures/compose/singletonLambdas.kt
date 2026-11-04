// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable lambda that captures nothing is created once, in a property of the file's
// `ComposableSingletons` object, and read from there; its body is a restartable, skippable group.
// FILE: singletonLambdas.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Frame(content: @Composable () -> Unit) {
    content()
}

@Composable
fun Panel(title: String, content: @Composable () -> Unit) {
    Label(title)
    content()
}

@Composable
fun Screen() {
    Frame { Label("one") }
    Panel("two") {
        Label("three")
    }
    Frame {
        Label("four")
        Label("five")
    }
}

@Composable
fun Label(text: String) {
    log.append(text).append(' ')
}

val log = StringBuilder()

// FILE: box.kt
package screens

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy

// A proxy composer records each group and skipping decision; the runtime's own
// `ComposableLambdaImpl` holds each singleton's body and calls it through the same composer.
fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

fun box(): String {
    var changed = true
    var first = true
    val scopes = ArrayList<(Composer, Int) -> Unit>()
    val scope = implement(ScopeUpdateScope::class.java) { _, _, args ->
        @Suppress("UNCHECKED_CAST")
        scopes.add(args[0] as (Composer, Int) -> Unit)
        null
    }
    val composer = implement(Composer::class.java) { proxy, method, args ->
        val name: String = method.name
        if (name != "sourceInformation") log.append(name).append(' ')
        when (name) {
            "startRestartGroup" -> proxy
            "sourceInformation", "skipToGroupEnd" -> null
            "changed", "changedInstance" -> changed
            // A first composition runs every group; a recomposition skips those whose inputs held.
            "shouldExecute" -> {
                val execute = args[0] as Boolean || first
                log.append(execute).append(' ')
                execute
            }
            "endRestartGroup" -> scope
            else -> throw IllegalStateException(name)
        }
    }
    val screen = Class.forName("screens.SingletonLambdasKt")
        .getMethod("Screen", Composer::class.java, Integer.TYPE)
    screen.invoke(null, composer, 1)
    log.append(scopes.size).append('|')
    changed = false
    first = false
    scopes.last()(composer, 0)
    log.append(scopes.size)
    // Recompose `Screen`: its singletons are the same instances, so every child skips.
    if (log.toString() != "startRestartGroup shouldExecute true startRestartGroup shouldExecute true " +
        "startRestartGroup changed shouldExecute true startRestartGroup shouldExecute " +
        "true one endRestartGroup endRestartGroup endRestartGroup startRestartGroup " +
        "shouldExecute true startRestartGroup shouldExecute true two endRestartGroup " +
        "startRestartGroup changed shouldExecute true startRestartGroup shouldExecute " +
        "true three endRestartGroup endRestartGroup endRestartGroup startRestartGroup " +
        "shouldExecute true startRestartGroup changed shouldExecute true " +
        "startRestartGroup shouldExecute true four endRestartGroup startRestartGroup " +
        "shouldExecute true five endRestartGroup endRestartGroup endRestartGroup " +
        "endRestartGroup 12|startRestartGroup shouldExecute true startRestartGroup " +
        "shouldExecute false skipToGroupEnd endRestartGroup startRestartGroup " +
        "shouldExecute false skipToGroupEnd endRestartGroup startRestartGroup " +
        "shouldExecute false skipToGroupEnd endRestartGroup endRestartGroup 16"
    ) return log.toString()
    return "OK"
}
