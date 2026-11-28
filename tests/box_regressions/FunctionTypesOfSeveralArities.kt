// One object implements function types of three different arities, and each is called through
// its own type: every call must reach the body of that arity.
class Counter : () -> String, (Int) -> String, (Int, Int) -> String {
    override fun invoke(): String = "zero"
    override fun invoke(a: Int): String = "one:$a"
    override fun invoke(a: Int, b: Int): String = "two:${a + b}"
}

fun callZero(f: () -> String) = f()
fun callOne(f: (Int) -> String) = f(7)
fun callTwo(f: (Int, Int) -> String) = f(3, 4)

fun box(): String {
    val counter = Counter()
    if (callZero(counter) != "zero") return "FAIL 1: ${callZero(counter)}"
    if (callOne(counter) != "one:7") return "FAIL 2: ${callOne(counter)}"
    if (callTwo(counter) != "two:7") return "FAIL 3: ${callTwo(counter)}"
    return "OK"
}
