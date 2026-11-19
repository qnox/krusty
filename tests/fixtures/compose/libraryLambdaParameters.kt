// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A call of a library composable with a `@Composable () -> Unit` parameter passes a composable
// lambda like a call of the file's own function does: a lambda that captures nothing is a
// singleton and one that captures a parameter is remembered, each compared by identity.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

val log = StringBuilder()

@Composable
fun Frame(content: @Composable () -> Unit) {
    log.append("frame;")
    content()
}

@Composable
fun Panel(title: String, content: @Composable () -> Unit) {
    log.append("panel $title;")
    content()
}

@Composable
fun Label(text: String) {
    log.append("label $text;")
}

// MODULE: main(lib)
// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import lib.Frame
import lib.Label
import lib.Panel

@Composable
fun Screen(title: String) {
    Frame { Label("static") }
    Panel(title) { Label("static") }
    Panel("fixed") { Label(title) }
}

// FILE: recorder.kt
package app

import androidx.compose.runtime.Composer
import lib.log
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
package app

import androidx.compose.runtime.Composer
import lib.log

fun box(): String {
    val screen = Class.forName("app.ScreenKt")
        .getMethod("Screen", String::class.java, Composer::class.java, Integer.TYPE)
    screen.invoke(null, "a", composer, 0)
    log.append(slots.size).append('|')
    val remembered = ArrayList(slots)
    slot = 0
    first = false
    screen.invoke(null, "b", composer, 0)
    log.append(slots.size).append(' ').append(remembered == slots)
    if (log.toString() !=
        "startRestartGroup changed shouldExecute true startRestartGroup shouldExecute true " +
        "frame;startRestartGroup changed shouldExecute true startRestartGroup shouldExecute " +
        "true label static;endRestartGroup endRestartGroup endRestartGroup startRestartGroup " +
        "shouldExecute true panel a;startRestartGroup changed shouldExecute true " +
        "startRestartGroup shouldExecute true label static;endRestartGroup endRestartGroup " +
        "endRestartGroup startRestartGroup shouldExecute true panel fixed;startRestartGroup " +
        "changed shouldExecute true startRestartGroup changed shouldExecute true label " +
        "a;endRestartGroup endRestartGroup endRestartGroup endRestartGroup 1|startRestartGroup " +
        "changed shouldExecute true startRestartGroup shouldExecute false skipToGroupEnd " +
        "endRestartGroup startRestartGroup shouldExecute true panel b;startRestartGroup " +
        "changed shouldExecute false skipToGroupEnd endRestartGroup endRestartGroup " +
        "startRestartGroup shouldExecute false skipToGroupEnd endRestartGroup endRestartGroup " +
        "1 true"
    ) return log.toString()
    return "OK"
}
