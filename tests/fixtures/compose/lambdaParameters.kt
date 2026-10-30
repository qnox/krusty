// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable `() -> Unit` parameter takes the caller's composer and `$changed` when called, is
// compared by identity, and passes on its own state bits; its applier scheme is inferred.
// FILE: lambdaParameters.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Frame(content: @Composable () -> Unit) {
    content()
}

@Composable
fun Panel(title: String, content: @Composable () -> Unit) {
    Label(title)
    Frame(content)
    content()
}

@Composable
fun Label(text: String) {
    println(text)
}

// FILE: box.kt
package screens

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy

// A proxy composer records each group, comparison and skipping decision the transformed functions
// make, and keeps every recompose scope.
fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

fun box(): String {
    val log = StringBuilder()
    var changed = true
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
            "shouldExecute" -> args[0].also { log.append(it).append(' ') }
            "endRestartGroup" -> scope
            else -> throw IllegalStateException(name)
        }
    }
    // The content a caller passes: a plain function value of the composable calling convention's
    // shape, which logs the bits it is called with.
    val content = { _: Any?, bits: Int ->
        log.append(bits).append(' ')
        Unit
    }
    val panel = Class.forName("screens.LambdaParametersKt").getMethod(
        "Panel",
        String::class.java,
        Function2::class.java,
        Composer::class.java,
        Integer.TYPE,
    )
    panel.invoke(null, "title", content, composer, 0)
    log.append(scopes.size).append('|')
    changed = false
    // Recompose `Panel` with its arguments the same as before: the restart forces its own group,
    // its children are skipped through the bits it passes on, and the content it calls itself
    // learns that its value is the same.
    scopes[2](composer, 0)
    log.append(scopes.size)
    if (log.toString() != "startRestartGroup changed changedInstance shouldExecute true " +
        "startRestartGroup shouldExecute true endRestartGroup startRestartGroup shouldExecute " +
        "true 4 endRestartGroup 4 endRestartGroup 3|startRestartGroup changed changedInstance " +
        "shouldExecute true startRestartGroup shouldExecute false skipToGroupEnd " +
        "endRestartGroup startRestartGroup shouldExecute false skipToGroupEnd endRestartGroup 2 " +
        "endRestartGroup 6"
    ) return log.toString()
    return "OK"
}
