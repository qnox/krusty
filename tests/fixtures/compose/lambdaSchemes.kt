// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_COMPOSE
// The applier schemes kotlinc infers for composable lambda parameters, whichever function is
// declared first, and lambda parameters beside defaulted and named arguments.
// FILE: lambdaSchemes.kt
package screens

import androidx.compose.runtime.Composable

@Composable
fun Both(a: @Composable () -> Unit, b: @Composable () -> Unit) {
    Swapped(b, a)
    Partly(b, a)
    Second(a, b)
    Forward(a)
    Ignore(b)
}

@Composable
fun Swapped(a: @Composable () -> Unit, b: @Composable () -> Unit) {
    Twice(b, a)
}

@Composable
fun Partly(a: @Composable () -> Unit, b: @Composable () -> Unit) {
    First(b, a)
    a()
}

@Composable
fun Second(a: @Composable () -> Unit, b: @Composable () -> Unit) {
    First(b, a)
}

@Composable
fun Forward(content: @Composable () -> Unit) {
    Frame(content = content)
}

@Composable
fun Ignore(content: @Composable () -> Unit) {
    Unused(content)
}

@Composable
fun Unused(content: @Composable () -> Unit) {
    println("unused")
}

@Composable
fun Twice(a: @Composable () -> Unit, b: @Composable () -> Unit) {
    a()
    b()
}

@Composable
fun First(a: @Composable () -> Unit, b: @Composable () -> Unit) {
    a()
}

@Composable
fun Frame(
    size: Int = 2,
    label: String = "frame",
    content: @Composable () -> Unit,
) {
    println(label + size)
    content()
}

// FILE: box.kt
package screens

import androidx.compose.runtime.Composer
import androidx.compose.runtime.ScopeUpdateScope
import java.lang.reflect.InvocationHandler
import java.lang.reflect.Proxy

fun <T> implement(type: Class<T>, handler: InvocationHandler): T =
    type.cast(Proxy.newProxyInstance(type.classLoader, arrayOf(type), handler))

fun box(): String {
    val log = StringBuilder()
    val scope = implement(ScopeUpdateScope::class.java) { _, _, _ -> null }
    // Every argument is new and every group runs.
    val composer = implement(Composer::class.java) { proxy, method, args ->
        val name: String = method.name
        when (name) {
            "startRestartGroup" -> proxy
            "endRestartGroup" -> scope
            "shouldExecute" -> args[0]
            "changed", "changedInstance" -> true
            else -> null
        }
    }
    // Each content logs its name and the bits it is called with.
    val a = { _: Any?, bits: Int -> log.append("a").append(bits).append(' '); Unit }
    val b = { _: Any?, bits: Int -> log.append("b").append(bits).append(' '); Unit }
    Class.forName("screens.LambdaSchemesKt")
        .getMethod("Both", Function2::class.java, Function2::class.java, Composer::class.java, Integer.TYPE)
        .invoke(null, a, b, composer, 0)
    return if (log.toString() == "a4 b4 a4 b4 b4 a4 ") "OK" else log.toString()
}
