// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// Composable functions with stable parameters compare the ones they read when the caller does not
// know their state, skip when every one is unchanged, and pass on what they know of each argument.
// FILE: trackedParameters.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Count(value: Int) {
    println(value)
}

@Composable
fun Title(text: String, unused: Long) {
    println(text)
}

@Composable
fun Card(title: String, count: Int, flag: Boolean) {
    Title(title, 7L)
    Count(count + 1)
    Count(3)
    if (flag) println(title)
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
        when (name) {
            "startRestartGroup" -> {
                log.append("start;")
                proxy
            }
            "sourceInformation" -> null
            "changed" -> {
                log.append("changed ").append(args[0]).append(';')
                changed
            }
            "shouldExecute" -> {
                log.append("execute ").append(args[0]).append(';')
                args[0]
            }
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
    val card = Class.forName("screens.TrackedParametersKt").getMethod(
        "Card",
        String::class.java,
        Integer.TYPE,
        java.lang.Boolean.TYPE,
        Composer::class.java,
        Integer.TYPE,
    )
    card.invoke(null, "title", 2, true, composer, 0)
    log.append(scopes.size)
    if (log.toString() != "start;changed title;changed 2;changed true;execute true;start;execute true;end;" +
        "start;changed 3;execute true;end;start;execute false;skip;end;end;4"
    ) return log.toString()
    log.setLength(0)
    changed = false
    // Recompose `Card` with every argument the same as before: the restart forces its own group,
    // and each call passes on what it knows, so every child group is skipped.
    scopes[3](composer, 0)
    log.append(scopes.size)
    if (log.toString() != "start;changed title;changed 2;changed true;execute true;start;execute false;" +
        "skip;end;start;changed 3;execute false;skip;end;start;execute false;skip;end;end;8"
    ) return log.toString()
    return "OK"
}
