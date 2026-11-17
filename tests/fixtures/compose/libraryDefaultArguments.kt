// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A call of a library composable that omits arguments passes a placeholder for each, then the
// caller's composer and `$changed`, then the `$default` mask of the omitted parameters: the callee
// assigns its own defaults and an omitted argument needs no comparison.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

val log = StringBuilder()

@Composable
fun Badge(label: String = "new", count: Int = 1, ratio: Double = 0.5) {
    log.append("$label $count $ratio;")
}

@Composable
fun Tag(name: String, size: Long = 2L) {
    log.append("$name $size;")
}

// MODULE: main(lib)
// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import lib.Badge
import lib.Tag

@Composable
fun Screen(title: String) {
    Badge()
    Badge(count = 5)
    Badge("x", ratio = 2.5)
    Badge(title)
    Tag(title)
    Tag("t", 9L)
}

// FILE: box.kt
package app

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy
import lib.log

fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

fun box(): String {
    val scope = implement(ScopeUpdateScope::class.java) { _, _, _ -> null }
    val composer = implement(Composer::class.java) { proxy, method, args ->
        val name: String = method.name
        when (name) {
            "startRestartGroup" -> proxy
            "endRestartGroup" -> scope
            "sourceInformation", "sourceInformationMarkerStart", "sourceInformationMarkerEnd" -> null
            "changed" -> {
                log.append("changed ${args[0]};")
                true
            }
            "getSkipping" -> false
            "shouldExecute" -> true
            else -> throw IllegalStateException(name)
        }
    }
    Class.forName("app.ScreenKt")
        .getMethod("Screen", String::class.java, Composer::class.java, Integer.TYPE)
        .invoke(null, "n", composer, 0)
    val expected = "changed n;new 1 0.5;new 5 0.5;x 1 2.5;n 1 0.5;n 2;t 9;"
    return if (log.toString() == expected) "OK" else log.toString()
}
