// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// Composable functions assign their constant default arguments inside their group and take a
// `$default` mask instead of a `$default` synthetic; an omitted argument needs no comparison.
// FILE: defaultArguments.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Badge(
    label: String = "new",
    count: Int = 1,
) {
    println(label + count)
}

@Composable
fun Card(title: String, size: Long = 2L) {
    Badge()
    Badge(count = 5)
    Badge(title)
    println(size)
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
    val card = Class.forName("screens.DefaultArgumentsKt").getMethod(
        "Card",
        String::class.java,
        java.lang.Long.TYPE,
        Composer::class.java,
        Integer.TYPE,
        Integer.TYPE,
    )
    // `Card("title")`, its `size` omitted.
    card.invoke(null, "title", 0L, composer, 0, 2)
    log.append(scopes.size)
    if (log.toString() != "start;changed title;execute true;start;execute false;skip;end;" +
        "start;execute false;skip;end;start;execute true;end;end;4"
    ) return log.toString()
    log.setLength(0)
    changed = false
    // Recompose `Card` with its argument the same as before: the restart forces its own group,
    // which assigns `size` again, and every child group is skipped.
    scopes[3](composer, 0)
    log.append(scopes.size)
    if (log.toString() != "start;changed title;execute true;start;execute false;skip;end;" +
        "start;execute false;skip;end;start;execute false;skip;end;end;8"
    ) return log.toString()
    return "OK"
}
