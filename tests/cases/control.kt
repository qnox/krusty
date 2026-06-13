fun max(a: Int, b: Int): Int = if (a > b) a else b
fun fib(n: Int): Int {
    var a = 0
    var b = 1
    var i = 0
    while (i < n) {
        val t = a + b
        a = b
        b = t
        i = i + 1
    }
    return a
}
