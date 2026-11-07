// TARGET_BACKEND: JVM_IR
// USE_INLINE_SCOPES_NUMBERS
// WITH_STDLIB
fun a(x: Int): Int = x.let { it + 1 }
fun b(x: Int): Int {
    val y = x.let { it + 1 }
    return y
}
fun d(s: String): Int = s.run { length }
fun e(x: Int): Int = x.let { a -> a.let { b -> b * 2 } }
fun f(s: String): String = s.also { println(it) }
fun g(numbers: List<Int>): Int {
    var sum = 0
    numbers.forEach { n -> sum += n.let { it * 2 } }
    return sum
}
fun box(): String = if (a(1) + b(1) + d("ab") + e(1) == 8 && f("q") == "q" && g(listOf(1, 2)) == 6) "OK" else "fail"
