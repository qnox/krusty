// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable lambda that captures its function's value parameters is remembered in the
// composition by its key, with the parameters it captures passed to its implementation; the
// remembered instance takes each new body, so a recomposition runs it with the current values.
// FILE: rememberedLambdas.kt
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
fun Card(title: String, count: Int) {
    Frame { Label(title) }
    Panel("static") {
        Label("count")
        log.append(count).append(' ')
    }
    Panel(title,
        {
            Label(title)
        })
}

@Composable
fun Label(text: String) {
    log.append(text).append(' ')
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
    val card = Class.forName("screens.RememberedLambdasKt")
        .getMethod("Card", String::class.java, Integer.TYPE, Composer::class.java, Integer.TYPE)
    card.invoke(null, "a", 1, composer, 0)
    log.append(slots.size).append('|')
    // Compose `Card` again with new arguments: it remembers the same lambda instances, and the
    // group whose own input changed runs with the new value.
    val remembered = ArrayList(slots)
    slot = 0
    first = false
    card.invoke(null, "b", 2, composer, 0)
    log.append(slots.size).append(' ').append(remembered == slots).append('|')
    // Recompose `Card` from its scope with unchanged inputs: every child skips.
    slot = 0
    changed = false
    @Suppress("UNCHECKED_CAST")
    (recompose as (Composer, Int) -> Unit)(composer, 0)
    if (log.toString() != "startRestartGroup changed changed shouldExecute true startRestartGroup " +
        "shouldExecute true startRestartGroup changed shouldExecute true " +
        "startRestartGroup changed shouldExecute true a endRestartGroup endRestartGroup " +
        "endRestartGroup startRestartGroup shouldExecute true startRestartGroup " +
        "shouldExecute true static endRestartGroup startRestartGroup changed " +
        "shouldExecute true startRestartGroup shouldExecute true count endRestartGroup 1 " +
        "endRestartGroup endRestartGroup startRestartGroup shouldExecute true " +
        "startRestartGroup shouldExecute true a endRestartGroup startRestartGroup changed " +
        "shouldExecute true startRestartGroup changed shouldExecute true a " +
        "endRestartGroup endRestartGroup endRestartGroup endRestartGroup " +
        "3|startRestartGroup changed changed shouldExecute true startRestartGroup " +
        "shouldExecute false skipToGroupEnd endRestartGroup startRestartGroup " +
        "shouldExecute false skipToGroupEnd endRestartGroup startRestartGroup " +
        "shouldExecute true startRestartGroup shouldExecute true b endRestartGroup " +
        "startRestartGroup changed shouldExecute false skipToGroupEnd endRestartGroup " +
        "endRestartGroup endRestartGroup 3 true|startRestartGroup changed changed " +
        "shouldExecute true startRestartGroup shouldExecute false skipToGroupEnd " +
        "endRestartGroup startRestartGroup shouldExecute false skipToGroupEnd " +
        "endRestartGroup startRestartGroup shouldExecute false skipToGroupEnd " +
        "endRestartGroup endRestartGroup "
    ) return log.toString()
    return "OK"
}
