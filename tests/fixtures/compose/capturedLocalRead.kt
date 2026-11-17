// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable lambda that only reads a captured local, here printing it, is remembered like one
// that captures a parameter.
// FILE: capturedLocalRead.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Frame(content: @Composable () -> Unit) {
    content()
}

@Composable
fun Greeting(text: String) {
    val shown = text
    Frame { println(shown) }
}

val log = StringBuilder()

// FILE: recorder.kt
package screens

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy

fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

val unlogged = setOf(
    "sourceInformation", "sourceInformationMarkerStart", "sourceInformationMarkerEnd",
    "getRecomposeScope", "rememberedValue", "updateRememberedValue",
)

// A proxy composer records each group and skipping decision and keeps the remembered values in
// slot order, as a composition's slot table does; the runtime's own `ComposableLambdaImpl` holds
// each remembered body and calls it through the same composer.
var changed = true
var first = true
val slots = ArrayList<Any?>()
var slot = 0
var recompose: Any? = null
private val scope = implement(ScopeUpdateScope::class.java) { _, _, args ->
    recompose = args[0]
    null
}
val composer: Composer = implement(Composer::class.java) { proxy, method, args ->
    val name: String = method.name
    if (name !in unlogged) log.append(name).append(' ')
    when {
        name == "startRestartGroup" -> proxy
        name == "rememberedValue" -> if (slot < slots.size) slots[slot++] else Composer.Empty
        name == "updateRememberedValue" -> {
            slots.add(args[0])
            slot++
            null
        }
        name == "changed" || name == "changedInstance" -> changed
        // A first composition runs every group; a recomposition skips those whose inputs held.
        name == "shouldExecute" -> {
            val execute = args[0] as Boolean || first
            log.append(execute).append(' ')
            execute
        }
        name == "endRestartGroup" -> scope
        name == "skipToGroupEnd" || name in unlogged -> null
        else -> throw IllegalStateException(name)
    }
}

// FILE: box.kt
package screens

import androidx.compose.runtime.Composer

fun box(): String {
    val greeting = Class.forName("screens.CapturedLocalReadKt")
        .getMethod("Greeting", String::class.java, Composer::class.java, Integer.TYPE)
    greeting.invoke(null, "a", composer, 0)
    log.append(slots.size).append('|')
    val remembered = ArrayList(slots)
    slot = 0
    first = false
    greeting.invoke(null, "b", composer, 0)
    log.append(slots.size).append(' ').append(remembered == slots)
    if (log.toString() !=
        "startRestartGroup changed shouldExecute true startRestartGroup shouldExecute true " +
        "startRestartGroup changed shouldExecute true endRestartGroup endRestartGroup " +
        "endRestartGroup 1|startRestartGroup changed shouldExecute true startRestartGroup " +
        "shouldExecute false skipToGroupEnd endRestartGroup endRestartGroup 1 true"
    ) return log.toString()
    return "OK"
}
