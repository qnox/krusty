// TARGET_BACKEND: JVM_IR
// WITH_STDLIB
// Every contextual one-parameter function type binds a lambda's implicit `it`: a typed
// declaration, a branch of one, a stdlib higher-order argument, a fun interface, a suspend function
// type, and an explicit type argument.
fun interface IntOp { fun apply(x: Int): Int }
fun <T> id(x: T): T = x
fun take(op: IntOp) = op.apply(2)
fun box(): String {
    val f: (Int) -> Int = { it * 2 }
    val g: ((Int) -> Int)? = if (f(1) > 0) { { it + 1 } } else null
    val h = listOf(1, 2).map { it + 1 }.filter { it > 2 }
    val k = IntOp { it + 3 }
    val n: suspend (Int) -> Int = { it }
    val o = id<(String) -> Int> { it.length }
    val all = listOf(f(1), g!!(1), h.single(), k.apply(1), take { it * 5 }, o("ab"))
    return if (all == listOf(2, 2, 3, 4, 10, 2) && n != null) "OK" else all.toString()
}
