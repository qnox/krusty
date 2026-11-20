// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// A library composable with more than ten parameters takes several `$changed` ints after the
// composer, ten parameters each, and a call passes the caller's knowledge of each argument in the
// int of its slot: a constant is static, a parameter passes on the caller's `$dirty` bits, and
// anything else leaves the callee to compare it.
// MODULE: lib
// FILE: lib.kt
package lib

import androidx.compose.runtime.Composable

val log = StringBuilder()

@Composable
fun Wide(a: Int, b: Int, c: Int, d: Int, e: Int, f: Int, g: Int, h: Int, i: Int, j: Int, k: String, l: Int) {
    log.append("$a$b$c$d$e$f$g$h$i$j$k$l;")
}

@Composable
fun Tail(a: Int, b: Int, c: Int, d: Int, e: Int, f: Int, g: Int, h: Int, i: Int, j: Int, k: String, l: Int) {
    log.append("$a$b;")
}

@Composable
fun Head(a: Int, b: Int, c: Int, d: Int, e: Int, f: Int, g: Int, h: Int, i: Int, j: Int, k: String, l: Int) {
    log.append("$k$l;")
}

// Defaults on the last parameters, which sit in the second `$changed` int.
@Composable
fun Styled(a: Int, b: Int, c: Int, d: Int, e: Int, f: Int, g: Int, h: Int, i: Int, j: Int, k: String = "d", l: Int = 5) {
    log.append("$a$k$l;")
}

// MODULE: main(lib)
// FILE: screen.kt
package app

import androidx.compose.runtime.Composable
import lib.Head
import lib.Styled
import lib.Tail
import lib.Wide

// Arguments forwarded from one int of the caller's parameters into another of the callee's.
@Composable
fun Relay(a: Int, b: Int, c: Int, d: Int, e: Int, f: Int, g: Int, h: Int, i: Int, j: Int, k: String, l: Int) {
    Wide(l, b, 3, 4, 5, 6, 7, 8, 9, a, k, 1)
}

@Composable
fun Screen(count: Int, name: String) {
    Wide(1, 2, 3, 4, 5, 6, 7, 8, 9, 0, "s", 1)
    Wide(count, 2, 3, 4, 5, 6, 7, 8, 9, count + 1, name, count)
    Wide(1, count, 3, 4, 5, 6, 7, 8, 9, 0, "t", 2)
    Tail(count, 2, 3, 4, 5, 6, 7, 8, 9, 0, name, 3)
    Head(count, 2, 3, 4, 5, 6, 7, 8, 9, 0, name, count)
    Styled(count, 2, 3, 4, 5, 6, 7, 8, 9, 0)
    Styled(1, 2, 3, 4, 5, 6, 7, 8, 9, 0, name)
    Relay(1, count, 3, 4, 5, 6, 7, 8, 9, 0, name, 4)
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
    val expected = "changed 7;changed n;1234567890s1;changed 8;7234567898n7;1734567890t2;72;n7;7d5;1n5;4734567891n1;"
    return if (log.toString() == expected) "OK" else log.toString()
}
