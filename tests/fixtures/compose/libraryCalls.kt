// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A composable function of a library the Compose compiler plugin compiled takes the caller's
// composer and `$changed` already, so a call passes them after its own arguments, with the caller's
// knowledge of each: a constant is static, a parameter passes on the caller's `$dirty` bits, and
// anything else leaves the callee to compare it.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

val log = StringBuilder()

@Composable
fun Greet(count: Int, name: String) {
    log.append("$count $name;")
}

@Composable
fun Empty() {
    log.append("empty;")
}

@Composable
fun Flags(on: Boolean, ratio: Double, code: Long, letter: Char) {
    log.append("$on $ratio $code $letter;")
}

// MODULE: main(lib)
// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import lib.Empty
import lib.Flags
import lib.Greet

@Composable
fun Screen(count: Int, name: String) {
    Greet(1, "a")
    Greet(count, name)
    Greet(count + 1, "b")
    Greet(2, name)
    Empty()
    Flags(true, 1.5, 3L, 'x')
    Frame { Empty() }
}

@Composable
fun Frame(content: @Composable () -> Unit) {
    content()
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

// A proxy composer runs every group and records which arguments the callees compare.
fun box(): String {
    val scope = implement(ScopeUpdateScope::class.java) { _, _, _ -> null }
    val composer = implement(Composer::class.java) { proxy, method, args ->
        val name: String = method.name
        when (name) {
            "startRestartGroup" -> proxy
            "endRestartGroup" -> scope
            "sourceInformation", "sourceInformationMarkerStart", "sourceInformationMarkerEnd" -> null
            "changed" -> {
                val value = args[0]
                log.append("changed ${if (value is Function<*>) "lambda" else value};")
                true
            }
            "getSkipping" -> false
            "shouldExecute" -> true
            else -> throw IllegalStateException(name)
        }
    }
    Class.forName("app.ScreenKt")
        .getMethod("Screen", Integer.TYPE, String::class.java, Composer::class.java, Integer.TYPE)
        .invoke(null, 7, "n", composer, 0)
    val expected = "changed 7;changed n;1 a;7 n;changed 8;8 b;2 n;empty;true 1.5 3 x;changed lambda;empty;"
    return if (log.toString() == expected) "OK" else log.toString()
}
